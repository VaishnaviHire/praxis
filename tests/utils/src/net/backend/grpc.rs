// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Praxis Contributors

//! HTTP/2 (h2c) gRPC backend for integration testing.
//!
//! gRPC lives on HTTP/2: the status of a call arrives in response
//! trailers, which no HTTP/1.1 leg can carry. This backend speaks h2c
//! with prior knowledge and answers every request with a length-prefixed
//! gRPC message followed by `grpc-status` trailers, so the proxy's
//! HTTP/2 upstream path can be exercised end to end.

use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use tokio::net::TcpListener;
use tracing::debug;

use crate::net::port::free_port;

// ---------------------------------------------------------------------------
// GrpcBackend
// ---------------------------------------------------------------------------

/// A canned gRPC response served over h2c.
#[derive(Clone, Debug)]
pub struct GrpcBackend {
    /// Response body, sent as a single DATA frame.
    ///
    /// Defaults to one empty length-prefixed gRPC message.
    body: Bytes,

    /// `content-type` sent on the response.
    content_type: String,

    /// `grpc-message` trailer, omitted when `None`.
    grpc_message: Option<String>,

    /// `grpc-status` trailer value.
    grpc_status: u32,

    /// HTTP status on the response HEADERS frame.
    http_status: u16,

    /// `grpc-status-details-bin` trailer, omitted when `None`.
    status_details_bin: Option<String>,

    /// Send a Trailers-Only response: one HEADERS frame carrying the
    /// gRPC status, with `END_STREAM` set and no body.
    trailers_only: bool,
}

impl Default for GrpcBackend {
    fn default() -> Self {
        Self {
            body: Bytes::from_static(&[0, 0, 0, 0, 0]),
            content_type: "application/grpc".to_owned(),
            grpc_message: None,
            grpc_status: 0,
            http_status: 200,
            status_details_bin: None,
            trailers_only: false,
        }
    }
}

impl GrpcBackend {
    /// A backend answering `OK` with an empty gRPC message.
    #[must_use]
    pub fn ok() -> Self {
        Self::default()
    }

    /// A backend answering with the given gRPC status code.
    #[must_use]
    pub fn status(code: u32) -> Self {
        Self {
            grpc_status: code,
            ..Self::default()
        }
    }

    /// Set the response body sent before the trailers.
    #[must_use]
    pub fn body<B: Into<Bytes>>(mut self, body: B) -> Self {
        self.body = body.into();
        self
    }

    /// Set the response `content-type`.
    #[must_use]
    pub fn content_type(mut self, value: &str) -> Self {
        value.clone_into(&mut self.content_type);
        self
    }

    /// Set the `grpc-message` trailer.
    #[must_use]
    pub fn message(mut self, message: &str) -> Self {
        self.grpc_message = Some(message.to_owned());
        self
    }

    /// Set the `grpc-status-details-bin` trailer.
    #[must_use]
    pub fn status_details(mut self, value: &str) -> Self {
        self.status_details_bin = Some(value.to_owned());
        self
    }

    /// Answer with a Trailers-Only response instead of headers + body + trailers.
    #[must_use]
    pub fn trailers_only(mut self) -> Self {
        self.trailers_only = true;
        self
    }

    /// Set the HTTP status on the response HEADERS frame.
    #[must_use]
    pub fn http_status(mut self, status: u16) -> Self {
        self.http_status = status;
        self
    }
}

// ---------------------------------------------------------------------------
// Guard
// ---------------------------------------------------------------------------

/// RAII guard for a running gRPC backend.
///
/// The server shuts down when the guard is dropped.
pub struct GrpcBackendGuard {
    /// The port the backend is listening on.
    port: u16,

    /// Shutdown signal sender.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl GrpcBackendGuard {
    /// The allocated port number.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for GrpcBackendGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _sent = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Start an h2c gRPC backend serving `backend` on a free port.
///
/// The server runs on its own thread with its own current-thread
/// runtime, so it can be started from a synchronous `#[test]`.
///
/// # Panics
///
/// Panics if the listener cannot bind.
#[must_use]
pub fn start_grpc_backend(backend: GrpcBackend) -> GrpcBackendGuard {
    let port = free_port();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let backend = Arc::new(backend);

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime for grpc backend");
        runtime.block_on(async move {
            let addr: SocketAddr = ([127, 0, 0, 1], port).into();
            let listener = TcpListener::bind(addr).await.expect("bind grpc backend");
            debug!(port, "grpc backend listening");
            let _ready = ready_tx.send(());
            tokio::select! {
                () = accept_loop(&listener, &backend) => {},
                _ = shutdown_rx => debug!(port, "grpc backend shutting down"),
            }
        });
    });

    // Block until the listener is bound so a test cannot race the backend.
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("grpc backend failed to bind");

    GrpcBackendGuard {
        port,
        shutdown: Some(shutdown_tx),
    }
}

/// Accept h2c connections until the task is cancelled.
#[expect(clippy::infinite_loop, reason = "server accept loop runs until task cancellation")]
async fn accept_loop(listener: &TcpListener, backend: &Arc<GrpcBackend>) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            continue;
        };
        debug!(%peer, "grpc backend accepted connection");
        let backend = Arc::clone(backend);
        tokio::spawn(async move {
            serve_connection(stream, &backend).await;
        });
    }
}

/// Serve every stream on one h2c connection.
async fn serve_connection(stream: tokio::net::TcpStream, backend: &GrpcBackend) {
    let Ok(mut connection) = h2::server::handshake(stream).await else {
        debug!("grpc backend h2 handshake failed");
        return;
    };
    while let Some(Ok((request, respond))) = connection.accept().await {
        let backend = backend.clone();
        tokio::spawn(async move {
            serve_stream(request, respond, &backend).await;
        });
    }
}

/// Answer one gRPC call.
async fn serve_stream(
    request: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    backend: &GrpcBackend,
) {
    // Drain the request body so flow control does not stall the client.
    let mut body = request.into_body();
    while let Some(Ok(chunk)) = body.data().await {
        let _release = body.flow_control().release_capacity(chunk.len());
    }

    let mut builder = http::Response::builder()
        .status(backend.http_status)
        .header("content-type", backend.content_type.as_str());
    if backend.trailers_only {
        builder = builder.header("grpc-status", backend.grpc_status.to_string());
        if let Some(message) = &backend.grpc_message {
            builder = builder.header("grpc-message", message.as_str());
        }
    }
    let Ok(response) = builder.body(()) else {
        return;
    };

    let Ok(mut send) = respond.send_response(response, backend.trailers_only) else {
        return;
    };
    if backend.trailers_only {
        return;
    }

    if send.send_data(backend.body.clone(), false).is_err() {
        return;
    }
    let _sent = send.send_trailers(build_trailers(backend));
}

/// Build the response trailers for a non-Trailers-Only answer.
fn build_trailers(backend: &GrpcBackend) -> http::HeaderMap {
    let mut trailers = http::HeaderMap::new();
    if let Ok(value) = http::HeaderValue::from_str(&backend.grpc_status.to_string()) {
        let _prev = trailers.insert("grpc-status", value);
    }
    if let Some(message) = &backend.grpc_message
        && let Ok(value) = http::HeaderValue::from_str(message)
    {
        let _prev = trailers.insert("grpc-message", value);
    }
    if let Some(details) = &backend.status_details_bin
        && let Ok(value) = http::HeaderValue::from_str(details)
    {
        let _prev = trailers.insert("grpc-status-details-bin", value);
    }
    trailers
}
