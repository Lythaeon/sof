#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::missing_docs_in_private_items,
        clippy::panic,
        missing_docs
    )
)]

//! Typed control surface for SOF gossip and ingest tuning.
//!
//! This crate is intentionally narrow:
//! - it models the tuning knobs SOF can already apply directly,
//! - it keeps upstream gossip queue ambitions explicit without pretending they are live,
//! - it gives service builders one typed place to define host-specific tuning presets.

mod constants;
mod error;
mod newtypes;
mod profiles;
mod types;

#[cfg(test)]
mod tests;

pub use constants::{
    DEFAULT_INGEST_QUEUE_CAPACITY, DEFAULT_RECEIVER_COALESCE_WAIT_MS, DEFAULT_UDP_BATCH_SIZE,
    LEGACY_GOSSIP_CHANNEL_CAPACITY, VPS_GOSSIP_CHANNEL_CAPACITY,
};
pub use error::TuningValueError;
pub use newtypes::{CpuCoreIndex, QueueCapacity, ReceiverCoalesceWindow, TvuReceiveSocketCount};
pub use types::{
    GossipChannelTuning, GossipTuningProfile, HostProfilePreset, IngestQueueMode,
    PendingGossipQueuePlan, ReceiverFanoutProfile, ReceiverPinningPolicy, SofRuntimeTuning,
};
