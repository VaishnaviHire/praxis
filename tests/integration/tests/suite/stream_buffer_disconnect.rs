// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Praxis Contributors

//! Regression test for issue #1194: a downstream client disconnect during
//! `StreamBuffer` request-body pre-read must be reported as a client error,
//! not an HTTP 500 server error.

use std::{
    io::{Read as _, Write as _},
    net::{Shutdown, TcpStream},
    time::Duration,
};

use praxis_core::config::Config;
use praxis_test_utils::{free_port, start_backend_with_shutdown, start_proxy};

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[test]
fn stream_buffer_client_disconnect_is_not_a_server_error() {
    let backend = start_backend_with_shutdown("disconnect-ok");
    let proxy_port = free_port();
    let yaml = format!(
        r#"
listeners:
  - name: default
    address: "127.0.0.1:{proxy_port}"
    filter_chains: [main]
filter_chains:
  - name: main
    filters:
      - filter: json_body_field
        field: model
        header: X-Model
      - filter: router
        routes:
          - path_prefix: "/"
            cluster: backend
      - filter: load_balancer
        clusters:
          - name: backend
            endpoints:
              - "127.0.0.1:{}"
insecure_options:
  allow_private_endpoints: true
"#,
        backend.port()
    );
    let config = Config::from_yaml(&yaml).unwrap();
    let proxy = start_proxy(&config);

    let mut stream = TcpStream::connect(proxy.addr()).expect("TCP connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    let request = concat!(
        "POST /api HTTP/1.1\r\n",
        "Host: localhost\r\n",
        "Content-Type: application/json\r\n",
        "Content-Length: 100000\r\n",
        "\r\n",
        "{\"model\":\"gpt",
    );
    stream
        .write_all(request.as_bytes())
        .expect("write partial request body");
    stream
        .shutdown(Shutdown::Write)
        .expect("half-close the write side to signal a premature end");

    let mut response = Vec::new();
    let _read = stream.read_to_end(&mut response);
    let response = String::from_utf8_lossy(&response);
    let status = parse_status(&response);

    assert!(
        (400..500).contains(&status),
        "a client disconnect during body pre-read must be a client error, not a 5xx; got {status}:\n{response}"
    );
    assert_eq!(
        status, 499,
        "a premature client close should surface as 499 Client Closed Request:\n{response}"
    );
}

// -----------------------------------------------------------------------------
// Test Utilities
// -----------------------------------------------------------------------------

/// Parse the numeric status code from an HTTP/1.1 response status line.
fn parse_status(response: &str) -> u16 {
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0)
}
