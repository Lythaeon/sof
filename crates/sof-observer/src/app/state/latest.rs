use super::*;

pub const fn note_latest_shred_slot(
    latest_shred_slot: &mut Option<u64>,
    latest_shred_updated_at: &mut Instant,
    slot: u64,
    observed_at: Instant,
) {
    match latest_shred_slot {
        Some(current) => {
            if slot > *current {
                *current = slot;
                *latest_shred_updated_at = observed_at;
            } else if slot == *current {
                // Keep stall detection aligned with live intake at the current tip slot.
                *latest_shred_updated_at = observed_at;
            }
        }
        None => {
            *latest_shred_slot = Some(slot);
            *latest_shred_updated_at = observed_at;
        }
    }
}
