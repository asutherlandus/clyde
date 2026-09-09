//! The one file the fixture edits and then reverts.
//!
//! Its content is deliberately load-bearing: `answer()` returns a value a test
//! can assert on, so "the build that ran matches the tree on disk" is checkable
//! rather than merely assumed.

pub fn answer() -> u32 {
    42
}
