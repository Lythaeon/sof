#![cfg_attr(
    not(feature = "agave-unstable-api"),
    deprecated(
        since = "3.1.0",
        note = "This crate has been marked for formal inclusion in the Agave Unstable API. From \
                v4.0.0 onward, the `agave-unstable-api` crate feature must be specified to \
                acknowledge use of an interface that may break without warning."
    )
)]
#![allow(clippy::arithmetic_side_effects)]
// Activate some of the Rust 2024 lints to make the future migration easier.
#![warn(if_let_rescope)]
#![warn(keyword_idents_2024)]
#![warn(missing_unsafe_on_extern)]
#![warn(rust_2024_guarded_string_incompatible_syntax)]
#![warn(rust_2024_incompatible_pat)]
#![warn(tail_expr_drop_order)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

use std::{collections::HashMap, sync::RwLock};

pub mod cluster_info;
pub mod cluster_info_metrics;
pub mod contact_info;
pub mod crds;
pub mod crds_data;
pub mod crds_entry;
mod crds_filter;
pub mod crds_gossip;
pub mod crds_gossip_error;
pub mod crds_gossip_pull;
pub mod crds_gossip_push;
pub mod crds_shards;
pub mod crds_value;
mod deprecated;
pub mod duplicate_shred;
#[cfg(feature = "duplicate-shred-rocksdb")]
pub mod duplicate_shred_handler;
#[cfg(feature = "duplicate-shred-rocksdb")]
pub mod duplicate_shred_listener;
pub mod epoch_slots;
pub mod epoch_specs;
pub mod gossip_error;
pub mod gossip_service;
pub mod node;
#[macro_use]
mod tlv;
#[macro_use]
mod legacy_contact_info;
pub mod ping_pong;
mod protocol;
mod push_active_set;
mod received_cache;
pub mod restart_crds_values;
pub mod weighted_shuffle;

#[macro_use]
extern crate log;

#[cfg(test)]
#[macro_use]
extern crate assert_matches;

#[macro_use]
extern crate solana_metrics;

mod wire_format_tests;

static RUNTIME_ENV_OVERRIDES: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

pub fn set_runtime_env_overrides(
    overrides: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) {
    let overrides = overrides
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    *RUNTIME_ENV_OVERRIDES.write().unwrap() = Some(overrides);
}

pub fn clear_runtime_env_overrides() {
    *RUNTIME_ENV_OVERRIDES.write().unwrap() = None;
}

pub(crate) fn read_runtime_env_override(name: &str) -> Option<String> {
    RUNTIME_ENV_OVERRIDES
        .read()
        .unwrap()
        .as_ref()
        .and_then(|overrides| overrides.get(name).cloned())
}

pub(crate) fn read_runtime_env_bool(name: &str) -> Option<bool> {
    read_runtime_env_override(name)
        .or_else(|| std::env::var(name).ok())
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
}
