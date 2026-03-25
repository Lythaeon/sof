//! Public constants for built-in SOF gossip and ingest tuning presets.

/// Built-in ingest queue capacity for the Home preset.
pub const HOME_INGEST_QUEUE_CAPACITY: u32 = 65_536;
/// Built-in UDP batch size for the Home preset.
pub const HOME_UDP_BATCH_SIZE: u16 = 64;
/// Built-in shred dedupe capacity for the Home preset.
pub const HOME_SHRED_DEDUP_CAPACITY: usize = 262_144;
/// Built-in gossip channel capacity for the Home preset.
pub const HOME_GOSSIP_CHANNEL_CAPACITY: u32 = 8_192;
/// Built-in gossip consume budget for the Home preset.
pub const HOME_GOSSIP_CHANNEL_CONSUME_CAPACITY: u32 = 1_024;
/// Built-in gossip worker count for the Home preset.
pub const HOME_GOSSIP_THREADS: usize = 2;
/// Built-in single-socket fanout for the Home preset.
pub const HOME_TVU_RECEIVE_SOCKETS: usize = 1;
/// Default coalesce wait used in SOF today.
pub const DEFAULT_RECEIVER_COALESCE_WAIT_MS: u64 = 1;
/// Default UDP batch size used in the current dedicated profile.
pub const DEFAULT_UDP_BATCH_SIZE: u16 = 128;
/// Default SOF ingest queue capacity for the lockfree queue.
pub const DEFAULT_INGEST_QUEUE_CAPACITY: u32 = 262_144;
/// UDP batch size validated on the public VPS profile.
pub const VPS_UDP_BATCH_SIZE: u16 = 96;
/// Shred dedupe capacity validated on the public VPS profile.
pub const VPS_SHRED_DEDUP_CAPACITY: usize = 524_288;
/// Built-in four-socket fanout for the current 4-core VPS preset.
pub const VPS_TVU_RECEIVE_SOCKETS: usize = 4;
/// Current upstream gossip channel capacity compiled into `solana-gossip`.
pub const LEGACY_GOSSIP_CHANNEL_CAPACITY: u32 = 4_096;
/// VPS gossip receiver queue target validated on constrained public hosts.
pub const VPS_GOSSIP_RECEIVER_CHANNEL_CAPACITY: u32 = 131_072;
/// VPS socket-consume and response queue target validated on constrained public hosts.
pub const VPS_SOCKET_CONSUME_CHANNEL_CAPACITY: u32 = 65_536;
/// VPS gossip consume budget validated on constrained public hosts.
pub const VPS_GOSSIP_CHANNEL_CONSUME_CAPACITY: u32 = 4_096;
/// Gossip worker count validated on constrained public hosts.
pub const VPS_GOSSIP_THREADS: usize = 4;
/// Built-in quad-socket fanout for the Dedicated preset.
pub const DEDICATED_TVU_RECEIVE_SOCKETS: usize = 4;
/// Built-in shred dedupe capacity for the Dedicated preset.
pub const DEDICATED_SHRED_DEDUP_CAPACITY: usize = 1_048_576;
/// Built-in gossip channel capacity for the Dedicated preset.
pub const DEDICATED_GOSSIP_CHANNEL_CAPACITY: u32 = 262_144;
/// Built-in socket-consume and response queue capacity for the Dedicated preset.
pub const DEDICATED_SOCKET_CONSUME_CHANNEL_CAPACITY: u32 = 131_072;
/// Built-in gossip consume budget for the Dedicated preset.
pub const DEDICATED_GOSSIP_CHANNEL_CONSUME_CAPACITY: u32 = 8_192;
/// Built-in gossip worker count for the Dedicated preset.
pub const DEDICATED_GOSSIP_THREADS: usize = 8;
