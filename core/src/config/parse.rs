// SPDX-License-Identifier: MIT
// Copyright (c) 2024 Praxis Contributors

//! YAML input safety checks: size limits and alias bomb guards.
//!
//! Prevents denial-of-service via crafted YAML by enforcing a raw
//! file size ceiling (`MAX_YAML_BYTES`, 4 MiB) and by rejecting YAML
//! alias nodes (`*anchor`) before the document is parsed. Aliases are
//! the mechanism behind "billion laughs" expansion, and a post-parse
//! size check cannot help: the expansion happens *inside* the parser,
//! so the memory blowup is already done by the time the result can be
//! measured. Praxis configs do not use YAML anchors/aliases, so
//! rejecting alias nodes up front removes the expansion vector entirely
//! without affecting any real configuration. (Anchors without a
//! matching alias expand nothing and are left alone.)

use std::path::Path;

use crate::errors::ProxyError;

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Maximum raw YAML input size (4 MiB).
const MAX_YAML_BYTES: usize = 4_194_304;

// -----------------------------------------------------------------------------
// Safety Checks
// -----------------------------------------------------------------------------

/// Reject a config file whose on-disk size exceeds `MAX_YAML_BYTES`.
///
/// Checks file metadata before reading, preventing memory exhaustion
/// from oversized files.
///
/// # Errors
///
/// Returns [`ProxyError::Config`] when the file is too large or its
/// metadata cannot be read.
///
/// [`ProxyError::Config`]: crate::errors::ProxyError::Config
pub(crate) fn check_file_size(path: &Path) -> Result<(), ProxyError> {
    let meta = std::fs::metadata(path).map_err(|e| {
        let display = path.display();
        ProxyError::Config(format!("failed to read metadata for {display}: {e}"))
    })?;

    let len = meta.len();
    let max = MAX_YAML_BYTES as u64;
    if len > max {
        return Err(ProxyError::Config(format!(
            "config file too large ({len} bytes, max {MAX_YAML_BYTES})"
        )));
    }
    Ok(())
}

/// Reject raw YAML input that exceeds `MAX_YAML_BYTES`.
///
/// # Errors
///
/// Returns [`ProxyError::Config`] when the input is too large.
///
/// ```ignore
/// use praxis_core::config::check_yaml_safety;
///
/// let small = "listeners: []";
/// check_yaml_safety(small).unwrap();
/// ```
///
/// [`ProxyError::Config`]: crate::errors::ProxyError::Config
pub(crate) fn check_yaml_safety(raw: &str) -> Result<(), ProxyError> {
    check_yaml_size(raw)?;
    reject_yaml_aliases(raw)
}

/// Reject raw YAML that exceeds the size limit.
///
/// # Errors
///
/// Returns [`ProxyError::Config`] when the input exceeds `MAX_YAML_BYTES`.
///
/// [`ProxyError::Config`]: crate::errors::ProxyError::Config
fn check_yaml_size(raw: &str) -> Result<(), ProxyError> {
    if raw.len() > MAX_YAML_BYTES {
        return Err(ProxyError::Config(format!(
            "YAML input too large ({} bytes, max {MAX_YAML_BYTES})",
            raw.len()
        )));
    }
    Ok(())
}

/// Reject YAML alias nodes (`*anchor`) before parsing.
///
/// Aliases drive "billion laughs" expansion, and the blowup happens
/// during `from_str` — so this must run before any parse. Praxis
/// configs never use aliases, so any alias node is rejected outright.
///
/// The scan is quote- and comment-aware so that a `*` inside a string
/// scalar (e.g. `pattern: "a*"`) or a `#` comment is not mistaken for
/// an alias. An alias node is a `*` at a value/node boundary followed
/// by an anchor-name character.
///
/// # Errors
///
/// Returns [`ProxyError::Config`] when an alias node is present.
///
/// [`ProxyError::Config`]: crate::errors::ProxyError::Config
fn reject_yaml_aliases(raw: &str) -> Result<(), ProxyError> {
    if let Some(line) = first_line_with_alias(raw) {
        return Err(ProxyError::Config(format!(
            "YAML alias nodes (`*anchor`) are not supported (line {line}); \
             they enable alias-expansion denial-of-service and are not used by any Praxis config"
        )));
    }
    Ok(())
}

/// Return the 1-based line number of the first YAML alias node, if any.
fn first_line_with_alias(raw: &str) -> Option<usize> {
    for (idx, line) in raw.lines().enumerate() {
        if line_contains_alias(line) {
            return Some(idx + 1);
        }
    }
    None
}

/// Whether a single line contains an alias node outside strings/comments.
///
/// Single-line scan only: YAML block scalars can span lines, but an
/// alias node itself is always single-line, and a false positive from a
/// `*` inside a multi-line block scalar would only reject an unusual
/// config, never admit a bomb.
fn line_contains_alias(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut at_boundary = true;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                at_boundary = false;
            },
            None => match c {
                b'#' => return false, // rest of line is a comment
                b'\'' | b'"' => {
                    quote = Some(c);
                    at_boundary = false;
                },
                b'*' if at_boundary => {
                    // Alias node when followed by an anchor-name char.
                    if bytes.get(i + 1).is_some_and(u8::is_ascii_alphanumeric) {
                        return true;
                    }
                    at_boundary = false;
                },
                _ => at_boundary = matches!(c, b' ' | b'\t' | b'[' | b'{' | b',' | b':' | b'-'),
            },
        }
        i += 1;
    }
    false
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
    clippy::needless_raw_strings,
    clippy::needless_raw_string_hashes,
    reason = "tests use unwrap/expect/indexing/raw strings for brevity"
)]
mod tests {
    use super::*;

    #[test]
    fn reject_oversized_yaml() {
        let huge = "x".repeat(5 * 1024 * 1024);
        let err = check_yaml_size(&huge).unwrap_err();
        assert!(err.to_string().contains("too large"), "should reject oversized YAML");
    }

    #[test]
    fn accept_small_yaml() {
        check_yaml_size("a: 1\n").expect("small YAML should pass size check");
    }

    #[test]
    fn reject_yaml_alias_bomb() {
        let err = reject_yaml_aliases("a: &a x\nb: &b [*a,*a,*a]\nlisteners: []\n");
        assert!(err.is_err(), "should reject alias nodes before parsing");
        assert!(
            err.unwrap_err().to_string().contains("alias nodes"),
            "error message should mention alias nodes"
        );
    }

    #[test]
    fn reject_single_alias() {
        let err = reject_yaml_aliases("a: &a x\nb: *a\nlisteners: []\n");
        assert!(err.is_err(), "any alias node should be rejected");
    }

    #[test]
    fn accept_anchor_without_alias() {
        // An anchor with no matching alias expands nothing and is allowed.
        reject_yaml_aliases("a: &a x\nlisteners: []\n").expect("unused anchor should pass");
    }

    #[test]
    fn accept_asterisk_in_string_and_comment() {
        reject_yaml_aliases("pattern: \"a*b\"\nglob: '*.txt'\nnote: ok # *not an alias\n")
            .expect("asterisks in strings/comments are not alias nodes");
    }

    #[test]
    fn accept_bare_asterisk_value() {
        // `*` not followed by an anchor-name char is not an alias node.
        reject_yaml_aliases("wildcard: /*\n").expect("glob-like value should pass");
    }

    #[test]
    fn safety_check_rejects_oversized() {
        let huge = "x".repeat(5 * 1024 * 1024);
        let err = check_yaml_safety(&huge).unwrap_err();
        assert!(err.to_string().contains("too large"), "should reject oversized YAML");
    }

    #[test]
    fn accept_yaml_at_exact_max_size() {
        let exact = "x".repeat(MAX_YAML_BYTES);
        check_yaml_size(&exact).expect("YAML at exactly MAX_YAML_BYTES should pass");
    }

    #[test]
    fn reject_yaml_one_byte_over_max() {
        let over = "x".repeat(MAX_YAML_BYTES + 1);
        let err = check_yaml_size(&over).unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[test]
    fn safety_check_passes_valid_yaml() {
        check_yaml_safety("a: 1\n").expect("valid small YAML should pass all safety checks");
    }

    #[test]
    fn alias_check_ignores_unparseable_non_alias_yaml() {
        // No alias node present; the real parse error is reported later.
        reject_yaml_aliases("{{{{invalid yaml").expect("non-alias garbage passes the alias check");
    }

    #[test]
    fn alias_line_number_reported() {
        let err = reject_yaml_aliases("listeners: []\nfoo: bar\nbomb: *a\n").unwrap_err();
        assert!(err.to_string().contains("line 3"), "got: {err}");
    }
}
