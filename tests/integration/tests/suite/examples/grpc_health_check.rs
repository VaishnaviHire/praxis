// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Praxis Contributors

//! Tests for the gRPC health check example configuration.
//!
//! The point of a gRPC probe is that it distinguishes "reachable" from
//! "serving", which neither the HTTP nor the TCP probe can do — so
//! these tests run the real probe against a backend that answers
//! `NOT_SERVING` while remaining perfectly reachable.

use std::{collections::HashMap, time::Duration};

use praxis_core::config::Config;
use praxis_test_utils::{
    GrpcBackend, free_port, http_get, http_send, parse_status, start_full_proxy, start_grpc_backend, wait_for_http,
};

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Load the example config, pointing it at two backends and a free
/// admin port.
fn example(proxy_port: u16, admin_port: u16, serving: u16, other: u16) -> Config {
    super::load_example_config(
        "observability/grpc-health-check.yaml",
        proxy_port,
        HashMap::from([
            ("127.0.0.1:50051", serving),
            ("127.0.0.1:50052", other),
            ("127.0.0.1:9901", admin_port),
        ]),
    )
}

/// Poll `/ready` until the cluster reports `expected` healthy
/// endpoints, or give up and return whatever it last said.
///
/// The example enables `admin.verbose`, which adds the per-cluster
/// healthy/unhealthy breakdown this needs.
fn wait_for_healthy_count(admin_addr: &str, expected: usize) -> String {
    let needle = format!("\"healthy\":{expected},");
    let mut last = String::new();
    for _ in 0..100 {
        let (_status, body) = http_get(admin_addr, "/ready", None);
        last = body;
        if last.contains("\"detail\"") && last.contains(&needle) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[test]
fn a_serving_backend_is_marked_healthy() {
    let serving = start_grpc_backend(GrpcBackend::ok().serving());
    let also_serving = start_grpc_backend(GrpcBackend::ok().serving());
    let proxy_port = free_port();
    let admin_port = free_port();
    let config = example(proxy_port, admin_port, serving.port(), also_serving.port());
    let _proxy = start_full_proxy(&config);

    let admin_addr = format!("127.0.0.1:{admin_port}");
    wait_for_http(&admin_addr);
    let body = wait_for_healthy_count(&admin_addr, 2);

    assert!(
        body.contains(r#""healthy":2,"unhealthy":0"#),
        "both SERVING backends should be marked healthy: {body}"
    );
}

#[test]
fn a_reachable_but_not_serving_backend_is_marked_unhealthy() {
    // The whole reason to prefer a gRPC probe: this backend completes
    // the HTTP/2 handshake and answers the call successfully, so an
    // `http` or `tcp` probe would call it healthy.
    let serving = start_grpc_backend(GrpcBackend::ok().serving());
    let not_serving = start_grpc_backend(GrpcBackend::ok().not_serving());
    let proxy_port = free_port();
    let admin_port = free_port();
    let config = example(proxy_port, admin_port, serving.port(), not_serving.port());
    let _proxy = start_full_proxy(&config);

    let admin_addr = format!("127.0.0.1:{admin_port}");
    wait_for_http(&admin_addr);
    let body = wait_for_healthy_count(&admin_addr, 1);

    assert!(
        body.contains(r#""healthy":1,"unhealthy":1"#),
        "the NOT_SERVING backend should be marked unhealthy despite being perfectly reachable: {body}"
    );
}

#[test]
fn a_backend_without_the_health_service_is_marked_unhealthy() {
    // No `.serving()`: the backend answers /grpc.health.v1.Health/Check
    // like any other method, which is what a server without the health
    // service registered does.
    let serving = start_grpc_backend(GrpcBackend::ok().serving());
    let no_health = start_grpc_backend(GrpcBackend::status(12).trailers_only());
    let proxy_port = free_port();
    let admin_port = free_port();
    let config = example(proxy_port, admin_port, serving.port(), no_health.port());
    let _proxy = start_full_proxy(&config);

    let admin_addr = format!("127.0.0.1:{admin_port}");
    wait_for_http(&admin_addr);
    let body = wait_for_healthy_count(&admin_addr, 1);

    assert!(
        body.contains(r#""healthy":1,"unhealthy":1"#),
        "an UNIMPLEMENTED health call means the probe cannot vouch for the backend: {body}"
    );
}

#[test]
fn traffic_still_flows_through_the_example() {
    let serving = start_grpc_backend(GrpcBackend::ok().serving().body(b"\x00\x00\x00\x00\x02hi".as_slice()));
    let also_serving = start_grpc_backend(GrpcBackend::ok().serving());
    let proxy_port = free_port();
    let admin_port = free_port();
    let config = example(proxy_port, admin_port, serving.port(), also_serving.port());
    let proxy = start_full_proxy(&config);

    let raw = http_send(
        proxy.addr(),
        "POST /pkg.Svc/Method HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/grpc\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\r\n",
    );

    assert_eq!(
        parse_status(&raw),
        200,
        "health checking must not get in the way of real traffic: {raw}"
    );
}
