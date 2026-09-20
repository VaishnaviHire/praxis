// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Praxis Contributors

// -----------------------------------------------------------------------------
// Reserved Headers
// -----------------------------------------------------------------------------

//! Reserved internal header utilities for proxy-owned routing metadata.
//!
//! Headers prefixed with `x-praxis-` and AI extension prefixes are
//! proxy-internal routing metadata that must never be forwarded to
//! clients or leaked from upstream responses. This module wraps the
//! core [`is_reserved`] check for use in the upstream response and
//! response filter hooks.
//!
//! [`is_reserved`]: praxis_core::reserved_headers::is_reserved

/// Return whether a header name belongs to Praxis reserved internal metadata.
///
/// Delegates to [`praxis_core::reserved_headers::is_reserved`] with
/// the lowercased name from [`http::HeaderName`].
///
/// [`praxis_core::reserved_headers::is_reserved`]: praxis_core::reserved_headers::is_reserved
pub(in crate::http::pingora::handler) fn is_reserved_internal_header(name: &http::HeaderName) -> bool {
    praxis_core::reserved_headers::is_reserved(name.as_str())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[expect(clippy::allow_attributes, reason = "blanket test suppressions")]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn delegates_to_core_is_reserved() {
        let reserved = http::HeaderName::from_static("x-praxis-foo");
        assert!(
            is_reserved_internal_header(&reserved),
            "wrapper must delegate to core for reserved headers"
        );

        let normal = http::HeaderName::from_static("x-custom-header");
        assert!(
            !is_reserved_internal_header(&normal),
            "wrapper must delegate to core for non-reserved headers"
        );
    }
}
