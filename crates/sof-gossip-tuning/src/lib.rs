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
//! - it keeps bundled gossip queue controls explicit and typed,
//! - it gives service builders one typed place to define host-specific tuning presets.

pub mod application;
pub mod domain;

#[cfg(test)]
mod tests;

pub use application::{ports::RuntimeTuningPort, service::GossipTuningService};
pub use domain::{
    constants::{
        DEFAULT_INGEST_QUEUE_CAPACITY, DEFAULT_RECEIVER_COALESCE_WAIT_MS, DEFAULT_UDP_BATCH_SIZE,
        LEGACY_GOSSIP_CHANNEL_CAPACITY, VPS_GOSSIP_CHANNEL_CAPACITY,
    },
    error::TuningValueError,
    model::{
        GossipChannelTuning, GossipTuningProfile, HostProfilePreset, IngestQueueMode,
        PendingGossipQueuePlan, ReceiverFanoutProfile, ReceiverPinningPolicy, SofRuntimeTuning,
    },
    value_objects::{CpuCoreIndex, QueueCapacity, ReceiverCoalesceWindow, TvuReceiveSocketCount},
};
