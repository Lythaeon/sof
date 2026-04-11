//! Shared internal support helpers for SOF workspace crates.

/// Benchmark helpers reused across profiling fixtures and microbenches.
pub mod bench;
/// Typed byte-slice conversion helpers reused across provider adapters.
pub mod bytes;
/// Collection helpers reused across runtime caches and provider adapters.
pub mod collections_support;
/// Environment parsing helpers reused across profiling fixtures and tests.
pub mod env_support;
/// Solana short-vector parsing helpers reused across serialized payload readers.
pub mod short_vec;
/// Duration and wall-clock helpers reused across transport adapters.
pub mod time_support;
