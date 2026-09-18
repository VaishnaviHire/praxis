// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Praxis Contributors

//! Shared gRPC protocol helpers.
//!
//! gRPC is carried over HTTP, so several Praxis layers need the same
//! answers about a request: is this gRPC, and which codec does it use?
//! The `grpc_detection` filter, the `grpc` condition predicate, and the
//! protocol layer all classify the same `content-type` header, so the
//! classification lives here rather than inside any one of them.

mod content_type;

pub use content_type::GrpcKind;
