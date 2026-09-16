// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Praxis Contributors

//! Shared gRPC protocol helpers.
//!
//! gRPC is carried over HTTP, so several Praxis layers need the same
//! answers about a request: is this gRPC, and which codec does it use?
//! The `grpc_detection` filter, the `grpc` condition predicate, and the
//! protocol layer all classify the same `content-type` header, so the
//! classification lives here rather than inside any one of them. The
//! same goes for a call's outcome, which the protocol layer reads from
//! response trailers and the access log renders.

mod content_type;
mod status;

pub use content_type::GrpcKind;
pub use status::{GrpcCompletion, GrpcStatusCode, UnknownGrpcStatusCode};
