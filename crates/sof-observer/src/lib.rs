#![recursion_limit = "256"]
#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::missing_docs_in_private_items,
        clippy::panic,
        missing_docs
    )
)]

//! SOF observer engine and plugin framework.
//!
//! External users should start from:
//! - [`crate::runtime`] to run the packaged observer loop.
//! - [`crate::framework`] to build observers using either:
//!   - [`crate::framework::ObserverPlugin`] + [`crate::framework::PluginHost`], or
//!   - [`crate::framework::RuntimeExtension`] + [`crate::framework::RuntimeExtensionHost`].

#[doc(hidden)]
mod app;
/// Runtime environment override storage used by code-driven setup APIs.
mod runtime_env;

/// Runtime transaction event types.
pub mod event;
/// Plugin framework for attaching custom runtime hooks.
pub mod framework;
#[doc(hidden)]
pub mod ingest;
#[doc(hidden)]
pub mod protocol;
#[doc(hidden)]
pub mod reassembly;
#[doc(hidden)]
pub mod relay;
#[doc(hidden)]
pub mod repair;
/// Packaged runtime entrypoints for embedding SOF.
pub mod runtime;
#[doc(hidden)]
pub mod shred;
#[doc(hidden)]
pub mod verify;
