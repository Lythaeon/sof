//! Public constants for built-in SOF gossip and ingest tuning presets.

/// Built-in ingest queue capacity for the Home preset.
pub const HOME_INGEST_QUEUE_CAPACITY: u32 = 65_536;
/// Built-in UDP batch size for the Home preset.
pub const HOME_UDP_BATCH_SIZE: u16 = 64;
/// Built-in gossip channel capacity for the Home preset.
pub const HOME_GOSSIP_CHANNEL_CAPACITY: u32 = 8_192;
/// Built-in single-socket fanout for the Home preset.
pub const HOME_TVU_RECEIVE_SOCKETS: usize = 1;
/// Default coalesce wait used in SOF today.
pub const DEFAULT_RECEIVER_COALESCE_WAIT_MS: u64 = 1;
/// Default UDP batch size used in the current VPS profile.
pub const DEFAULT_UDP_BATCH_SIZE: u16 = 128;
/// Default SOF ingest queue capacity for the lockfree queue.
pub const DEFAULT_INGEST_QUEUE_CAPACITY: u32 = 262_144;
/// Built-in dual-socket fanout for the VPS preset.
pub const VPS_TVU_RECEIVE_SOCKETS: usize = 2;
/// Current Agave gossip channel default observed upstream.
pub const LEGACY_GOSSIP_CHANNEL_CAPACITY: u32 = 4_096;
/// Current widened capacity that proved materially better on constrained VPS hosts.
pub const VPS_GOSSIP_CHANNEL_CAPACITY: u32 = 32_768;
/// Built-in quad-socket fanout for the Dedicated preset.
pub const DEDICATED_TVU_RECEIVE_SOCKETS: usize = 4;
/// Built-in gossip channel capacity for the Dedicated preset.
pub const DEDICATED_GOSSIP_CHANNEL_CAPACITY: u32 = 65_536;
/// Built-in socket-consume and response queue capacity for the Dedicated preset.
pub const DEDICATED_SOCKET_CONSUME_CHANNEL_CAPACITY: u32 = 32_768;
