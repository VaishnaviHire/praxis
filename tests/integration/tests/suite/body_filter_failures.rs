// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Praxis Contributors

//! Integration tests for body-filter failure handling: response body
//! rejections, filter errors, and stream-buffer ceilings on proxied
//! responses.

use std::sync::Arc;

use bytes::Bytes;
use praxis_core::config::Config;
use praxis_filter::{
    BodyAccess, BodyMode, FilterAction, FilterError, FilterFactory, FilterRegistry, HttpFilter, HttpFilterContext,
    Rejection,
};
use praxis_test_utils::{Backend, free_port, http_get, start_full_proxy_with_registry};

// ---------------------------------------------------------------------------
// Custom filters
// ---------------------------------------------------------------------------

/// Response-body filter that rejects once the full body has streamed.
struct RejectResponseBodyFilter;

#[async_trait::async_trait]
impl HttpFilter for RejectResponseBodyFilter {
    fn name(&self) -> &'static str {
        "test_reject_response_body"
    }

    fn response_body_access(&self) -> BodyAccess {
        BodyAccess::ReadOnly
    }

    async fn on_request(&self, _ctx: &mut HttpFilterContext<'_>) -> Result<FilterAction, FilterError> {
        Ok(FilterAction::Continue)
    }

    fn on_response_body(
        &self,
        _ctx: &mut HttpFilterContext<'_>,
        _body: &mut Option<Bytes>,
        end_of_stream: bool,
    ) -> Result<FilterAction, FilterError> {
        if end_of_stream {
            Ok(FilterAction::Reject(Rejection::status(451)))
        } else {
            Ok(FilterAction::Continue)
        }
    }
}

/// Response-body filter that errors on every chunk.
struct ErrorResponseBodyFilter;

#[async_trait::async_trait]
impl HttpFilter for ErrorResponseBodyFilter {
    fn name(&self) -> &'static str {
        "test_error_response_body"
    }

    fn response_body_access(&self) -> BodyAccess {
        BodyAccess::ReadOnly
    }

    async fn on_request(&self, _ctx: &mut HttpFilterContext<'_>) -> Result<FilterAction, FilterError> {
        Ok(FilterAction::Continue)
    }

    fn on_response_body(
        &self,
        _ctx: &mut HttpFilterContext<'_>,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
    ) -> Result<FilterAction, FilterError> {
        Err("test_error_response_body: body inspection failed".to_owned().into())
    }
}

/// Response-body filter that buffers with a tiny StreamBuffer ceiling.
struct TinyBufferResponseFilter;

#[async_trait::async_trait]
impl HttpFilter for TinyBufferResponseFilter {
    fn name(&self) -> &'static str {
        "test_tiny_buffer_response"
    }

    fn response_body_access(&self) -> BodyAccess {
        BodyAccess::ReadOnly
    }

    fn response_body_mode(&self) -> BodyMode {
        BodyMode::StreamBuffer { max_bytes: Some(8) }
    }

    async fn on_request(&self, _ctx: &mut HttpFilterContext<'_>) -> Result<FilterAction, FilterError> {
        Ok(FilterAction::Continue)
    }
}

/// Request-body filter that errors on every chunk.
struct ErrorRequestBodyFilter;

#[async_trait::async_trait]
impl HttpFilter for ErrorRequestBodyFilter {
    fn name(&self) -> &'static str {
        "test_error_request_body"
    }

    fn request_body_access(&self) -> BodyAccess {
        BodyAccess::ReadOnly
    }

    async fn on_request(&self, _ctx: &mut HttpFilterContext<'_>) -> Result<FilterAction, FilterError> {
        Ok(FilterAction::Continue)
    }

    async fn on_request_body(
        &self,
        _ctx: &mut HttpFilterContext<'_>,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
    ) -> Result<FilterAction, FilterError> {
        Err("test_error_request_body: body inspection failed".to_owned().into())
    }
}

/// Registry with builtins plus the failing body filters.
fn registry() -> FilterRegistry {
    let mut reg = FilterRegistry::with_builtins();
    reg.register(
        "test_reject_response_body",
        FilterFactory::Http(Arc::new(|_| Ok(Box::new(RejectResponseBodyFilter)))),
    )
    .unwrap();
    reg.register(
        "test_error_response_body",
        FilterFactory::Http(Arc::new(|_| Ok(Box::new(ErrorResponseBodyFilter)))),
    )
    .unwrap();
    reg.register(
        "test_tiny_buffer_response",
        FilterFactory::Http(Arc::new(|_| Ok(Box::new(TinyBufferResponseFilter)))),
    )
    .unwrap();
    reg.register(
        "test_error_request_body",
        FilterFactory::Http(Arc::new(|_| Ok(Box::new(ErrorRequestBodyFilter)))),
    )
    .unwrap();
    reg
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Proxy YAML with the given filter ahead of router + load balancer.
fn proxy_yaml(proxy_port: u16, backend_port: u16, filter_name: &str) -> String {
    format!(
        r#"
listeners:
  - name: web
    address: "127.0.0.1:{proxy_port}"
    filter_chains: [main]

insecure_options:
  allow_private_endpoints: true
  allow_private_upstreams: true

filter_chains:
  - name: main
    filters:
      - filter: {filter_name}
      - filter: router
        routes:
          - path_prefix: "/"
            cluster: backend
      - filter: load_balancer
        clusters:
          - name: backend
            endpoints:
              - "127.0.0.1:{backend_port}"
"#
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn response_body_rejection_does_not_deliver_body() {
    let backend_port = Backend::fixed("sensitive-response-content").start();
    let proxy_port = free_port();
    let config = Config::from_yaml(&proxy_yaml(proxy_port, backend_port, "test_reject_response_body")).unwrap();
    let reg = registry();
    let _proxy = start_full_proxy_with_registry(&config, &reg);

    let (_status, body) = http_get(&format!("127.0.0.1:{proxy_port}"), "/", None);
    assert!(
        !body.contains("sensitive-response-content"),
        "a rejected response body must not reach the client: {body:?}"
    );
}

#[test]
fn response_body_filter_error_does_not_deliver_body() {
    let backend_port = Backend::fixed("confidential-payload").start();
    let proxy_port = free_port();
    let config = Config::from_yaml(&proxy_yaml(proxy_port, backend_port, "test_error_response_body")).unwrap();
    let reg = registry();
    let _proxy = start_full_proxy_with_registry(&config, &reg);

    let (_status, body) = http_get(&format!("127.0.0.1:{proxy_port}"), "/", None);
    assert!(
        !body.contains("confidential-payload"),
        "an errored response body must not reach the client: {body:?}"
    );
}

#[test]
fn response_over_stream_buffer_ceiling_is_not_delivered() {
    let big_body = "x".repeat(64);
    let backend_port = Backend::fixed(&big_body).start();
    let proxy_port = free_port();
    let config = Config::from_yaml(&proxy_yaml(proxy_port, backend_port, "test_tiny_buffer_response")).unwrap();
    let reg = registry();
    let _proxy = start_full_proxy_with_registry(&config, &reg);

    let (_status, body) = http_get(&format!("127.0.0.1:{proxy_port}"), "/", None);
    assert!(
        !body.contains(&big_body),
        "a response over the StreamBuffer ceiling must not reach the client: {body:?}"
    );
}

#[test]
fn request_body_filter_error_rejects_request() {
    let backend_port = Backend::fixed("should-not-be-reached").start();
    let proxy_port = free_port();
    let config = Config::from_yaml(&proxy_yaml(proxy_port, backend_port, "test_error_request_body")).unwrap();
    let reg = registry();
    let _proxy = start_full_proxy_with_registry(&config, &reg);

    let raw = praxis_test_utils::http_send(
        &format!("127.0.0.1:{proxy_port}"),
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody",
    );
    assert!(
        !raw.contains("should-not-be-reached"),
        "an errored request body must not be forwarded: {raw:?}"
    );
}
