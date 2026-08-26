// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2024 Praxis Contributors

//! W3C Trace Context propagation filter.
//!
//! Initializes request-scoped [`TraceContext`] and injects `x-request-id`
//! and `traceparent` on the forwarded hop. Sub-requests pick up the same
//! context via [`HttpFilterContext::apply_trace_propagation`].
//!
//! Request ID resolution prefers a pending `request_id` filter value over
//! the inbound header, then falls back to the inbound header, then generates
//! a new ID. This keeps the generated `request_id` filter value authoritative
//! when both filters are enabled.
//!
//! # Limitations
//!
//! This filter is header propagation only: the hop `parent-id` is not
//! exported as a span, so tracing backends show the proxy as a missing
//! node. Deployments with the `otel` feature should use that span
//! context instead. New traces are always flagged sampled (`01`) because
//! the filter cannot consult sampler configuration.

use std::borrow::Cow;

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::{
    FilterAction, FilterError,
    factory::parse_filter_config,
    filter::{HttpFilter, HttpFilterContext},
    trace_context::{InboundTrace, REQUEST_ID_HEADER, TRACEPARENT_HEADER, TraceContext, parse_traceparent},
};

// -----------------------------------------------------------------------------
// Config
// -----------------------------------------------------------------------------

/// Configuration for the trace context propagation filter.
///
/// Currently accepts no fields; reserved for future options such as
/// trusted-header policies or sampling flag overrides.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::empty_structs_with_brackets,
    reason = "brackets required for serde mapping deserialization"
)]
struct TraceContextFilterConfig {}

// -----------------------------------------------------------------------------
// TraceContextFilter
// -----------------------------------------------------------------------------

/// Propagates W3C Trace Context and `x-request-id` correlation.
///
/// Per W3C Trace Context section 3.3.1.1, `tracestate` is forwarded only
/// when inbound `traceparent` is valid.
///
/// # YAML configuration
///
/// ```yaml
/// filter: trace_context
/// ```
///
/// # Example
///
/// ```ignore
/// use praxis_filter::TraceContextFilter;
///
/// let yaml: serde_yaml::Value = serde_yaml::from_str("{}").unwrap();
/// let filter = TraceContextFilter::from_config(&yaml).unwrap();
/// assert_eq!(filter.name(), "trace_context");
/// ```
pub struct TraceContextFilter;

impl TraceContextFilter {
    /// Create a trace context filter from parsed YAML config.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] if the YAML config is malformed.
    ///
    /// [`FilterError`]: crate::FilterError
    pub fn from_config(config: &serde_yaml::Value) -> Result<Box<dyn HttpFilter>, FilterError> {
        let _cfg: TraceContextFilterConfig = parse_filter_config("trace_context", config)?;
        Ok(Box::new(Self))
    }
}

#[async_trait]
impl HttpFilter for TraceContextFilter {
    fn name(&self) -> &'static str {
        "trace_context"
    }

    async fn on_request(&self, ctx: &mut HttpFilterContext<'_>) -> Result<FilterAction, FilterError> {
        if reuse_existing_trace_context(ctx) {
            return Ok(FilterAction::Continue);
        }

        let incoming = ctx
            .request
            .headers
            .get(TRACEPARENT_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_traceparent);

        let tc = initialize_trace_context(ctx, incoming.as_ref());
        inject_correlation_headers(ctx, &tc);
        ctx.extensions.insert(tc);

        // W3C 3.3.1.1: do not forward tracestate without a valid traceparent.
        if incoming.is_some() {
            forward_tracestate(ctx);
        } else {
            strip_tracestate(ctx);
        }

        Ok(FilterAction::Continue)
    }
}

/// Reuse an already-initialized request-scoped trace context.
fn reuse_existing_trace_context(ctx: &mut HttpFilterContext<'_>) -> bool {
    // A previous lifecycle phase may already have created the request-scoped
    // context. Keep already-pending forwarded-hop headers when they match the
    // shared request context. In particular, do not mint a fresh traceparent
    // just to compare with an existing pending value: a fresh span-id would be
    // expected to differ and would produce a misleading competing-header warning.
    let Some(tc) = ctx.extensions.get::<TraceContext>().cloned() else {
        return false;
    };
    let request_id = tc.request_id().to_owned();
    let trace_id = tc.trace_id().to_owned();

    ensure_extra_header(ctx, REQUEST_ID_HEADER, &request_id);
    ensure_traceparent_header(ctx, &tc, &trace_id);
    warn_competing_request_id(ctx, &request_id);
    true
}

/// Build the request-scoped trace context from inbound trace data or a new trace.
fn initialize_trace_context(ctx: &HttpFilterContext<'_>, incoming: Option<&InboundTrace>) -> TraceContext {
    let request_id = resolve_request_id(ctx);
    if let Some(trace) = incoming {
        debug!(
            trace_id = %trace.trace_id,
            flags = %trace.flags,
            "joining existing trace"
        );
        TraceContext::from_inbound(request_id, trace)
    } else {
        let context = TraceContext::new_sampled(request_id, ctx.id_generator, ctx.time_source);
        debug!(trace_id = %context.trace_id(), "starting new trace");
        context
    }
}

/// Inject correlation headers for the forwarded upstream request.
fn inject_correlation_headers(ctx: &mut HttpFilterContext<'_>, tc: &TraceContext) {
    // Initial setup path: remove untrusted downstream copies, add the
    // framework-owned values, and warn if existing pending extras would keep
    // a different request id in place. The reuse path performs the same check
    // independently after request extensions are already populated.
    let request_id = tc.request_id().to_owned();
    let [_, (_, traceparent)] = tc.headers_for_hop(ctx.id_generator, ctx.time_source);

    ctx.request_headers_to_remove
        .push(http::header::HeaderName::from_static(TRACEPARENT_HEADER));
    ctx.request_headers_to_remove
        .push(http::header::HeaderName::from_static(REQUEST_ID_HEADER));

    ensure_extra_header(ctx, REQUEST_ID_HEADER, &request_id);
    ensure_extra_header(ctx, TRACEPARENT_HEADER, &traceparent);
    warn_competing_request_id(ctx, &request_id);
}

// -----------------------------------------------------------------------------
// Request ID resolution
// -----------------------------------------------------------------------------

/// Resolve the request ID for the shared trace context.
///
/// Precedence is:
///
/// 1. a pending value from an earlier `request_id` filter,
/// 2. the inbound `x-request-id` header,
/// 3. a newly generated ID.
///
/// Pending values win over inbound values so the existing `request_id` filter
/// stays authoritative when both filters are configured. A conflict is logged
/// and resolved by keeping the pending value.
fn resolve_request_id(ctx: &HttpFilterContext<'_>) -> String {
    let inbound = ctx
        .request
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let pending = pending_request_id(ctx);

    match (pending, inbound) {
        (Some(pending), Some(inbound)) if pending != inbound => {
            warn!(
                pending = %pending,
                inbound = %inbound,
                "competing x-request-id values; preferring pending request_id filter value"
            );
            pending
        },
        (Some(pending), _) => pending,
        (None, Some(inbound)) => inbound,
        (None, None) => ctx.id_generator.generate(ctx.time_source),
    }
}

/// Return the first pending `x-request-id` value scheduled for upstream injection.
fn pending_request_id(ctx: &HttpFilterContext<'_>) -> Option<String> {
    let values: Vec<&str> = ctx
        .extra_request_headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(REQUEST_ID_HEADER))
        .map(|(_, value)| value.as_str())
        .collect();
    match values.as_slice() {
        [] => None,
        [only] => Some((*only).to_owned()),
        [first, rest @ ..] => {
            if rest.iter().any(|v| *v != *first) {
                warn!(
                    first = %first,
                    "multiple distinct pending x-request-id values; using the first"
                );
            }
            Some((*first).to_owned())
        },
    }
}

// -----------------------------------------------------------------------------
// Header injection helpers
// -----------------------------------------------------------------------------

/// Leave a competing pending extra in place rather than duplicating it.
fn ensure_extra_header(ctx: &mut HttpFilterContext<'_>, name: &'static str, value: &str) {
    let existing: Vec<String> = ctx
        .extra_request_headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .collect();

    if existing.is_empty() {
        ctx.extra_request_headers.push((Cow::Borrowed(name), value.to_owned()));
        return;
    }

    for existing_value in &existing {
        if existing_value != value {
            warn!(
                header = name,
                existing = %existing_value,
                expected = %value,
                "competing correlation header pending; leaving existing value in place"
            );
        }
    }
}

/// Ensure the forwarded hop has a `traceparent` without warning on expected span-id drift.
fn ensure_traceparent_header(ctx: &mut HttpFilterContext<'_>, tc: &TraceContext, expected_trace_id: &str) {
    let existing: Vec<String> = ctx
        .extra_request_headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(TRACEPARENT_HEADER))
        .map(|(_, value)| value.clone())
        .collect();

    if existing.is_empty() {
        let [_, (_, traceparent)] = tc.headers_for_hop(ctx.id_generator, ctx.time_source);
        ctx.extra_request_headers
            .push((Cow::Borrowed(TRACEPARENT_HEADER), traceparent));
        return;
    }

    for value in existing {
        match parse_traceparent(&value) {
            Some(parsed) if parsed.trace_id == expected_trace_id => {},
            _ => warn!(
                existing = %value,
                expected_trace_id = %expected_trace_id,
                "competing traceparent pending alongside TraceContext"
            ),
        }
    }
}

/// Warn when pending forwarded request headers disagree with the shared request ID.
fn warn_competing_request_id(ctx: &HttpFilterContext<'_>, expected: &str) {
    for (name, value) in &ctx.extra_request_headers {
        if name.eq_ignore_ascii_case(REQUEST_ID_HEADER) && value != expected {
            warn!(
                existing = %value,
                expected = %expected,
                "competing x-request-id pending alongside TraceContext"
            );
        }
    }
}

/// Forward inbound `tracestate` when the inbound `traceparent` was valid.
fn forward_tracestate(ctx: &mut HttpFilterContext<'_>) {
    let values: Vec<&str> = ctx
        .request
        .headers
        .get_all("tracestate")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();

    if values.is_empty() {
        return;
    }

    let combined = values.join(", ");
    debug!(tracestate = %combined, "forwarding tracestate");
    ctx.request_headers_to_remove
        .push(http::header::HeaderName::from_static("tracestate"));
    ensure_extra_header(ctx, "tracestate", &combined);
}

/// Remove inbound `tracestate` when there is no valid inbound `traceparent`.
fn strip_tracestate(ctx: &mut HttpFilterContext<'_>) {
    ctx.request_headers_to_remove
        .push(http::header::HeaderName::from_static("tracestate"));
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[expect(clippy::allow_attributes, reason = "blanket test suppressions")]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use praxis_core::subrequest::FrameworkHeaders;

    use super::*;

    // -------------------------------------------------------------------------
    // Filter lifecycle
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn generates_new_trace_and_request_id_when_absent() {
        let filter = make_filter("");
        let req = crate::test_utils::make_request(http::Method::GET, "/");
        let mut ctx = crate::test_utils::make_filter_context(&req);

        let action = filter.on_request(&mut ctx).await.unwrap();
        assert!(matches!(action, FilterAction::Continue));

        let tc = ctx.extensions.get::<TraceContext>().expect("TraceContext stored");
        assert_eq!(tc.request_id().len(), 32);
        assert_eq!(tc.flags(), "01");

        let traceparent = find_extra_header(&ctx, "traceparent").expect("traceparent injected");
        let tp = parse_traceparent(&traceparent).expect("well-formed");
        assert_eq!(tp.flags, "01");
        assert_eq!(
            find_extra_header(&ctx, "x-request-id").as_deref(),
            Some(tc.request_id())
        );
    }

    #[tokio::test]
    async fn joins_existing_trace_with_valid_traceparent() {
        let filter = make_filter("");
        let mut req = crate::test_utils::make_request(http::Method::GET, "/");
        req.headers.insert(
            http::header::HeaderName::from_static("traceparent"),
            http::header::HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());

        let traceparent = find_extra_header(&ctx, "traceparent").unwrap();
        let tp = parse_traceparent(&traceparent).unwrap();
        assert_eq!(tp.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        let parts: Vec<&str> = traceparent.split('-').collect();
        assert_ne!(parts[2], "00f067aa0ba902b7");
        assert_eq!(tp.flags, "01");
    }

    #[tokio::test]
    async fn malformed_and_all_zero_traceparent_fall_back_to_new_trace() {
        for bad in [
            "garbage-value",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
        ] {
            let filter = make_filter("");
            let mut req = crate::test_utils::make_request(http::Method::GET, "/");
            req.headers.insert(
                http::header::HeaderName::from_static("traceparent"),
                http::header::HeaderValue::from_str(bad).unwrap(),
            );
            let mut ctx = crate::test_utils::make_filter_context(&req);
            drop(filter.on_request(&mut ctx).await.unwrap());
            let traceparent = find_extra_header(&ctx, "traceparent").unwrap();
            let tp = parse_traceparent(&traceparent).unwrap();
            assert!(tp.flags == "01", "fallback trace should be sampled for {bad}");
            assert_ne!(tp.trace_id, "00000000000000000000000000000000");
        }
    }

    #[tokio::test]
    async fn masks_reserved_flags_on_join() {
        let filter = make_filter("");
        let mut req = crate::test_utils::make_request(http::Method::GET, "/");
        req.headers.insert(
            http::header::HeaderName::from_static("traceparent"),
            http::header::HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-03"),
        );
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());
        let traceparent = find_extra_header(&ctx, "traceparent").unwrap();
        assert!(
            traceparent.ends_with("-01"),
            "reserved bits must be masked: {traceparent}"
        );
    }

    #[tokio::test]
    async fn future_version_accepted_emits_version_00() {
        let filter = make_filter("");
        let mut req = crate::test_utils::make_request(http::Method::GET, "/");
        req.headers.insert(
            http::header::HeaderName::from_static("traceparent"),
            http::header::HeaderValue::from_static("02-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra"),
        );
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());
        let traceparent = find_extra_header(&ctx, "traceparent").unwrap();
        assert!(traceparent.starts_with("00-"));
        assert!(traceparent.contains("4bf92f3577b34da6a3ce929d0e0e4736"));
    }

    #[tokio::test]
    async fn reuses_pending_request_id_from_earlier_filter() {
        let filter = make_filter("");
        let req = crate::test_utils::make_request(http::Method::GET, "/");
        let mut ctx = crate::test_utils::make_filter_context(&req);
        ctx.extra_request_headers
            .push((Cow::Borrowed("x-request-id"), "from-request-id-filter".into()));
        drop(filter.on_request(&mut ctx).await.unwrap());

        let tc = ctx.extensions.get::<TraceContext>().unwrap();
        assert_eq!(tc.request_id(), "from-request-id-filter");
        assert_eq!(
            ctx.extra_request_headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("x-request-id"))
                .count(),
            1,
            "must not duplicate pending x-request-id"
        );
    }

    #[tokio::test]
    async fn pending_request_id_wins_over_conflicting_inbound_header() {
        let filter = make_filter("");
        let mut req = crate::test_utils::make_request(http::Method::GET, "/");
        req.headers.insert("x-request-id", "client-request-id".parse().unwrap());
        let mut ctx = crate::test_utils::make_filter_context(&req);
        ctx.extra_request_headers
            .push((Cow::Borrowed("x-request-id"), "from-request-id-filter".into()));

        drop(filter.on_request(&mut ctx).await.unwrap());

        let tc = ctx.extensions.get::<TraceContext>().unwrap();
        assert_eq!(tc.request_id(), "from-request-id-filter");
        assert_eq!(
            find_extra_header(&ctx, "x-request-id").as_deref(),
            Some("from-request-id-filter")
        );
    }

    #[tokio::test]
    async fn idempotent_on_request_does_not_duplicate_headers() {
        let filter = make_filter("");
        let req = crate::test_utils::make_request(http::Method::GET, "/");
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());
        let first_count = ctx.extra_request_headers.len();
        drop(filter.on_request(&mut ctx).await.unwrap());
        assert_eq!(
            ctx.extra_request_headers.len(),
            first_count,
            "second on_request must not duplicate pending headers"
        );
        assert_eq!(
            ctx.extra_request_headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("traceparent"))
                .count(),
            1
        );
        assert_eq!(
            ctx.extra_request_headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("x-request-id"))
                .count(),
            1
        );
    }

    #[test]
    fn ensure_extra_header_preserves_existing_competing_value() {
        let req = crate::test_utils::make_request(http::Method::GET, "/");
        let mut ctx = crate::test_utils::make_filter_context(&req);
        ctx.extra_request_headers
            .push((Cow::Borrowed("traceparent"), "existing-traceparent".into()));

        ensure_extra_header(&mut ctx, "traceparent", "new-traceparent");

        let values: Vec<_> = ctx
            .extra_request_headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("traceparent"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(values, vec!["existing-traceparent"]);
    }

    #[tokio::test]
    async fn competing_pending_request_id_is_detected() {
        let filter = make_filter("");
        let req = crate::test_utils::make_request(http::Method::GET, "/");
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());
        let expected = ctx.extensions.get::<TraceContext>().unwrap().request_id().to_owned();
        ctx.extra_request_headers
            .push((Cow::Borrowed("x-request-id"), "later-competing-id".into()));
        warn_competing_request_id(&ctx, &expected);
        assert_eq!(
            ctx.extensions.get::<TraceContext>().unwrap().request_id(),
            expected,
            "competing extras are warn-only; TraceContext stays authoritative"
        );
    }

    #[tokio::test]
    async fn apply_trace_propagation_injects_fresh_span_same_trace() {
        let filter = make_filter("");
        let req = crate::test_utils::make_request(http::Method::GET, "/");
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());

        let primary = find_extra_header(&ctx, "traceparent").unwrap();
        let primary_tp = parse_traceparent(&primary).unwrap();

        let mut fw = FrameworkHeaders::new();
        ctx.apply_trace_propagation(&mut fw);
        let fw_tp = fw
            .iter()
            .find(|(n, _)| n.as_str() == "traceparent")
            .map(|(_, v)| v.to_str().unwrap().to_owned())
            .expect("framework traceparent");
        let fw_parsed = parse_traceparent(&fw_tp).unwrap();
        assert_eq!(fw_parsed.trace_id, primary_tp.trace_id);
        assert_ne!(
            fw_tp.split("-").nth(2).unwrap(),
            primary.split("-").nth(2).unwrap(),
            "each outbound hop must mint a fresh span id"
        );
        let fw_rid = fw
            .iter()
            .find(|(n, _)| n.as_str() == "x-request-id")
            .map(|(_, v)| v.to_str().unwrap().to_owned())
            .unwrap();
        assert_eq!(fw_rid, ctx.extensions.get::<TraceContext>().unwrap().request_id());
    }

    #[tokio::test]
    async fn forwards_tracestate_when_traceparent_valid() {
        let filter = make_filter("");
        let mut req = crate::test_utils::make_request(http::Method::GET, "/");
        req.headers.insert(
            http::header::HeaderName::from_static("traceparent"),
            http::header::HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        req.headers.insert(
            http::header::HeaderName::from_static("tracestate"),
            http::header::HeaderValue::from_static("congo=t61rcWkgMzE,rojo=00f067aa0ba902b7"),
        );
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());
        assert_eq!(
            find_extra_header(&ctx, "tracestate").as_deref(),
            Some("congo=t61rcWkgMzE,rojo=00f067aa0ba902b7")
        );
    }

    #[tokio::test]
    async fn strips_tracestate_when_traceparent_invalid() {
        let filter = make_filter("");
        let mut req = crate::test_utils::make_request(http::Method::GET, "/");
        req.headers.insert(
            http::header::HeaderName::from_static("traceparent"),
            http::header::HeaderValue::from_static("garbage"),
        );
        req.headers.insert(
            http::header::HeaderName::from_static("tracestate"),
            http::header::HeaderValue::from_static("congo=t61rcWkgMzE"),
        );
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());
        assert!(find_extra_header(&ctx, "tracestate").is_none());
        assert!(ctx.request_headers_to_remove.iter().any(|h| h.as_str() == "tracestate"));
    }

    #[tokio::test]
    async fn preserves_unsampled_flag() {
        let filter = make_filter("");
        let mut req = crate::test_utils::make_request(http::Method::GET, "/");
        req.headers.insert(
            http::header::HeaderName::from_static("traceparent"),
            http::header::HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00"),
        );
        let mut ctx = crate::test_utils::make_filter_context(&req);
        drop(filter.on_request(&mut ctx).await.unwrap());
        let traceparent = find_extra_header(&ctx, "traceparent").unwrap();
        assert!(traceparent.ends_with("-00"));
        assert_eq!(ctx.extensions.get::<TraceContext>().unwrap().flags(), "00");
    }

    #[test]
    fn from_config_empty_and_null_succeed() {
        let config = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        assert_eq!(
            TraceContextFilter::from_config(&config).unwrap().name(),
            "trace_context"
        );
        assert_eq!(
            TraceContextFilter::from_config(&serde_yaml::Value::Null)
                .unwrap()
                .name(),
            "trace_context"
        );
    }

    #[test]
    fn from_config_rejects_unknown_fields() {
        let config: serde_yaml::Value = serde_yaml::from_str("bogus: true").unwrap();
        assert!(TraceContextFilter::from_config(&config).is_err());
    }

    // -------------------------------------------------------------------------
    // Test utilities
    // -------------------------------------------------------------------------

    fn make_filter(yaml: &str) -> TraceContextFilter {
        let config: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let _cfg: TraceContextFilterConfig = parse_filter_config("trace_context", &config).unwrap();
        TraceContextFilter
    }

    fn find_extra_header(ctx: &HttpFilterContext<'_>, name: &str) -> Option<String> {
        ctx.extra_request_headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }
}
