//! Public constants for built-in SOF gossip and ingest tuning presets.

/// Default coalesce wait used in SOF today.
pub const DEFAULT_RECEIVER_COALESCE_WAIT_MS: u64 = 1;
/// Default UDP batch size used in the current VPS profile.
pub const DEFAULT_UDP_BATCH_SIZE: u16 = 128;
/// Default SOF ingest queue capacity for the lockfree queue.
pub const DEFAULT_INGEST_QUEUE_CAPACITY: u32 = 262_144;
/// Current Agave gossip channel default observed upstream.
pub const LEGACY_GOSSIP_CHANNEL_CAPACITY: u32 = 4_096;
/// Current widened capacity that proved materially better on constrained VPS hosts.
pub const VPS_GOSSIP_CHANNEL_CAPACITY: u32 = 32_768;
