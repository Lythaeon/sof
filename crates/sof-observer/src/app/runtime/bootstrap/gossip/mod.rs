mod entrypoints;
mod handoff;
mod receiver;
#[cfg(feature = "gossip-bootstrap")]
mod switch;

pub(crate) use receiver::{ReceiverBootstrapError, start_receiver};
#[cfg(feature = "gossip-bootstrap")]
pub(crate) use switch::maybe_switch_gossip_runtime;

#[cfg(feature = "gossip-bootstrap")]
use super::relay::resolve_socket_addr;
use super::relay::{maybe_start_relay_server, read_relay_connect_addrs};
use super::*;
#[cfg(feature = "gossip-bootstrap")]
use entrypoints::{
    collect_runtime_switch_entrypoints, prioritize_gossip_entrypoints, probe_gossip_entrypoint_live,
};
#[cfg(feature = "gossip-bootstrap")]
use handoff::{
    build_gossip_runtime_port_plan, format_port_range, is_bind_conflict_error,
    start_gossip_bootstrapped_receiver_guarded, stop_gossip_runtime_components,
    wait_for_runtime_stabilization,
};
