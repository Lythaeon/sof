mod coverage;
mod dedupe;
mod latest;
mod repair;

pub use coverage::SlotCoverageWindow;
pub use dedupe::RecentShredCache;
pub use latest::note_latest_shred_slot;
pub use repair::OutstandingRepairRequests;

pub(super) use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

#[cfg(any(feature = "gossip-bootstrap", test))]
pub(super) use crate::repair::MissingShredRequest;
pub(super) use crate::{repair::MissingShredRequestKind, shred::wire::ParsedShredHeader};
