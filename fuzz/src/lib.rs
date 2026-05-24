//! Stub crate root for the cargo-fuzz harness.
//!
//! farbfeld is decode-only-fuzzed against arbitrary attacker bytes —
//! there's no system library to dlopen and cross-decode against, so this
//! `lib.rs` carries no interop. cargo-fuzz still requires a valid
//! library target for the `[[bin]]` fuzz-target entries to link
//! against, hence this placeholder.

pub fn _placeholder() {}
