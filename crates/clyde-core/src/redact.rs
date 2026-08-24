//! Redaction wrapper for sensitive values.
//!
//! Phase 0 establishes the pattern that credential-bearing types do not reveal
//! their content through `Debug`, `Display`, or `Serialize` (Phase 0 deliverable
//! 4). Phase 4's broker types are built on it.
//!
//! The wrapper deliberately does not implement `Serialize`: a type that cannot
//! be serialised cannot end up in an audit payload, a log line, or an API
//! response by accident, which is stronger than remembering to skip a field.

use std::fmt;

/// A value whose content must not appear in logs, errors, or audit payloads.
///
/// Access to the inner value is explicit and greppable via [`Redacted::expose`].
pub struct Redacted<T> {
    value: T,
    label: &'static str,
}

impl<T> Redacted<T> {
    /// Wraps a value. `label` names the kind of secret for diagnostics and is
    /// the only thing about it that is ever printed.
    pub fn new(value: T, label: &'static str) -> Self {
        Self { value, label }
    }

    /// Deliberately explicit accessor. Every call site is an auditable point
    /// where a secret is used.
    pub fn expose(&self) -> &T {
        &self.value
    }

    /// Consumes the wrapper, yielding the secret.
    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Transforms the inner value without widening exposure.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Redacted<U> {
        Redacted {
            value: f(self.value),
            label: self.label,
        }
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Redacted({} elided)", self.label)
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} redacted>", self.label)
    }
}

/// A byte buffer that is overwritten before it is freed.
///
/// Best-effort in safe Rust: the allocation is zeroed on drop, but nothing
/// prevents the compiler or allocator from having copied it earlier. It narrows
/// the window rather than closing it. Compose it as `Redacted<SecretBytes>` when
/// a secret is held as bytes.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes({} bytes elided)", self.0.len())
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    #[test]
    fn debug_and_display_do_not_reveal_content() {
        let secret = Redacted::new("hunter2".to_owned(), "model api key");
        let debug = format!("{secret:?}");
        let display = format!("{secret}");
        assert!(!debug.contains("hunter2"), "Debug leaked: {debug}");
        assert!(!display.contains("hunter2"), "Display leaked: {display}");
        assert!(debug.contains("model api key"));
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn nested_in_a_struct_debug_stays_redacted() {
        #[derive(Debug)]
        #[allow(dead_code, reason = "fields exist so the derived Debug renders them")]
        struct Holder {
            name: &'static str,
            token: Redacted<String>,
        }
        let holder = Holder {
            name: "session",
            token: Redacted::new("tok_abc".to_owned(), "session token"),
        };
        let rendered = format!("{holder:?}");
        assert!(rendered.contains("session"));
        assert!(!rendered.contains("tok_abc"), "leaked: {rendered}");
    }

    #[test]
    fn map_preserves_the_label() {
        let secret = Redacted::new(SecretBytes::new(vec![1u8, 2, 3]), "key material");
        let mapped = secret.map(|bytes| bytes.len());
        assert_eq!(mapped.label(), "key material");
        assert_eq!(*mapped.expose(), 3);
    }

    #[test]
    fn secret_bytes_debug_does_not_reveal_content() {
        let secret = SecretBytes::new(vec![0xde, 0xad, 0xbe, 0xef]);
        let rendered = format!("{secret:?}");
        assert!(rendered.contains("4 bytes"));
        assert!(!rendered.contains("222"), "leaked: {rendered}");
    }
}
