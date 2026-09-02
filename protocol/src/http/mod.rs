// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2024 Praxis Contributors

//! HTTP protocol implementation.

/// Pingora-backed HTTP implementation.
pub mod pingora;

pub use pingora::{PingoraHttp, handler::load_http_handler};
#[cfg(feature = "admin-api")]
pub use pingora::health::PingoraHealthService;
