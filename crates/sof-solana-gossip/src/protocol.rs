//! Definitions for the base of all Gossip protocol messages
use {
    crate::{
        crds_data::{CrdsData, MAX_WALLCLOCK},
        crds_gossip_pull::CrdsFilter,
        crds_value::CrdsValue,
        ping_pong::{self, Pong},
    },
    arrayvec::ArrayVec,
    bincode::serialize,
    ed25519_dalek::{verify_batch, Signature as DalekSignature, VerifyingKey},
    serde::{Deserialize, Serialize},
    solana_keypair::signable::Signable,
    solana_perf::packet::PACKET_DATA_SIZE,
    solana_pubkey::Pubkey,
    solana_sanitize::{Sanitize, SanitizeError},
    solana_signature::Signature,
    std::{
        borrow::Cow,
        collections::HashMap,
        fmt::Debug,
        result::Result,
        sync::OnceLock,
    },
};

pub(crate) const MAX_CRDS_OBJECT_SIZE: usize = 928;
/// Max size of serialized crds-values in a Protocol::PushMessage packet. This
/// is equal to PACKET_DATA_SIZE minus serialized size of an empty push
/// message: Protocol::PushMessage(Pubkey::default(), Vec::default())
pub(crate) const PUSH_MESSAGE_MAX_PAYLOAD_SIZE: usize = PACKET_DATA_SIZE - 44;
/// Max size of serialized crds-values in a Protocol::PullResponse packet. This
/// is equal to PACKET_DATA_SIZE minus serialized size of an empty pull
/// message: Protocol::PullResponse(Pubkey::default(), Vec::default())
pub(crate) const PULL_RESPONSE_MAX_PAYLOAD_SIZE: usize = PUSH_MESSAGE_MAX_PAYLOAD_SIZE;
#[cfg(feature = "duplicate-shred-rocksdb")]
pub(crate) const DUPLICATE_SHRED_MAX_PAYLOAD_SIZE: usize = PACKET_DATA_SIZE - 115;
/// Maximum number of incremental hashes in SnapshotHashes a node publishes
/// such that the serialized size of the push/pull message stays below
/// PACKET_DATA_SIZE.
pub(crate) const MAX_INCREMENTAL_SNAPSHOT_HASHES: usize = 25;
/// Maximum number of origin nodes that a PruneData may contain, such that the
/// serialized size of the PruneMessage stays below PACKET_DATA_SIZE.
pub(crate) const MAX_PRUNE_DATA_NODES: usize = 32;
/// Prune data prefix for PruneMessage
const PRUNE_DATA_PREFIX: &[u8] = b"\xffSOLANA_PRUNE_DATA";
/// Number of bytes in the randomly generated token sent with ping messages.
const GOSSIP_PING_TOKEN_SIZE: usize = 32;
/// Minimum serialized size of a Protocol::PullResponse packet.
pub(crate) const PULL_RESPONSE_MIN_SERIALIZED_SIZE: usize = 161;
const DEFAULT_GOSSIP_BATCH_VERIFY_MIN_VALUES: usize = 8;
const GOSSIP_BATCH_VERIFY_MIN_VALUES_ENV: &str = "SOF_GOSSIP_BATCH_VERIFY_MIN_VALUES";

// TODO These messages should go through the gpu pipeline for spam filtering
/// Gossip protocol messages base enum
#[derive(Serialize, Deserialize, Debug)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Protocol {
    PullRequest(CrdsFilter, CrdsValue),
    PullResponse(Pubkey, Vec<CrdsValue>),
    PushMessage(Pubkey, Vec<CrdsValue>),
    // TODO: Remove the redundant outer pubkey here,
    // and use the inner PruneData.pubkey instead.
    PruneMessage(Pubkey, PruneData),
    PingMessage(Ping),
    PongMessage(Pong),
    // Update count_packets_received if new variants are added here.
}

pub(crate) type Ping = ping_pong::Ping<GOSSIP_PING_TOKEN_SIZE>;
pub(crate) type PingCache = ping_pong::PingCache<GOSSIP_PING_TOKEN_SIZE>;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct PruneData {
    /// Pubkey of the node that sent this prune data
    pub(crate) pubkey: Pubkey,
    /// Pubkeys of nodes that should be pruned
    pub(crate) prunes: Vec<Pubkey>,
    /// Signature of this Prune Message
    pub(crate) signature: Signature,
    /// The Pubkey of the intended node/destination for this message
    pub(crate) destination: Pubkey,
    /// Wallclock of the node that generated this message
    pub(crate) wallclock: u64,
}

struct PreparedCrdsValue {
    verifying_key: VerifyingKey,
    signature: DalekSignature,
    buffer: ArrayVec<u8, PACKET_DATA_SIZE>,
}

enum VerifyingKeyCache {
    Inline(ArrayVec<(Pubkey, Option<VerifyingKey>), 8>),
    Heap(HashMap<Pubkey, Option<VerifyingKey>>),
}

impl VerifyingKeyCache {
    fn new() -> Self {
        Self::Inline(ArrayVec::new())
    }

    fn get_or_compute(&mut self, pubkey: Pubkey) -> Option<VerifyingKey> {
        match self {
            Self::Inline(entries) => {
                if let Some((_, verifying_key)) =
                    entries.iter().find(|(cached_pubkey, _)| *cached_pubkey == pubkey)
                {
                    return *verifying_key;
                }
                let verifying_key = compute_verifying_key(pubkey);
                if entries.try_push((pubkey, verifying_key)).is_ok() {
                    return verifying_key;
                }
                let mut map = HashMap::with_capacity(entries.len().saturating_add(1));
                let previous_entries = std::mem::take(entries);
                for (cached_pubkey, cached_verifying_key) in previous_entries {
                    let _ = map.insert(cached_pubkey, cached_verifying_key);
                }
                let _ = map.insert(pubkey, verifying_key);
                *self = Self::Heap(map);
                verifying_key
            }
            Self::Heap(entries) => *entries
                .entry(pubkey)
                .or_insert_with(|| compute_verifying_key(pubkey)),
        }
    }
}

fn compute_verifying_key(pubkey: Pubkey) -> Option<VerifyingKey> {
    let pubkey_bytes = pubkey.to_bytes();
    VerifyingKey::from_bytes(&pubkey_bytes)
        .ok()
        .filter(|verifying_key| !verifying_key.is_weak())
}

impl Protocol {
    fn verify_crds_values(values: &[CrdsValue]) -> bool {
        let Some(prepared_values) = Self::prepare_crds_values(values) else {
            return false;
        };
        if prepared_values.len() >= gossip_batch_verify_min_values()
            && Self::verify_crds_values_batch(&prepared_values)
        {
            return true;
        }
        Self::verify_crds_values_strict(&prepared_values)
    }

    fn prepare_crds_values(values: &[CrdsValue]) -> Option<Vec<PreparedCrdsValue>> {
        let mut verifying_keys = VerifyingKeyCache::new();
        let mut prepared_values = Vec::with_capacity(values.len());
        for value in values {
            prepared_values.push(Self::prepare_crds_value(value, &mut verifying_keys)?);
        }
        Some(prepared_values)
    }

    fn prepare_crds_value(
        value: &CrdsValue,
        verifying_keys: &mut VerifyingKeyCache,
    ) -> Option<PreparedCrdsValue> {
        let Some(verifying_key) = verifying_keys.get_or_compute(value.pubkey()) else {
            return None;
        };
        let signature = DalekSignature::from_bytes(value.signature().as_array());
        let mut buffer = ArrayVec::<u8, PACKET_DATA_SIZE>::new();
        bincode::serialize_into(&mut buffer, value.data()).ok()?;
        Some(PreparedCrdsValue {
            verifying_key,
            signature,
            buffer,
        })
    }

    fn verify_crds_values_strict(values: &[PreparedCrdsValue]) -> bool {
        for value in values {
            if value
                .verifying_key
                .verify_strict(&value.buffer, &value.signature)
                .is_err()
            {
                return false;
            }
        }
        true
    }

    fn verify_crds_values_batch(values: &[PreparedCrdsValue]) -> bool {
        let mut messages = Vec::with_capacity(values.len());
        let mut signatures = Vec::with_capacity(values.len());
        let mut verifying_keys = Vec::with_capacity(values.len());
        for value in values {
            messages.push(value.buffer.as_slice());
            signatures.push(value.signature);
            verifying_keys.push(value.verifying_key);
        }
        verify_batch(&messages, &signatures, &verifying_keys).is_ok()
    }

    /// Returns the bincode serialized size (in bytes) of the Protocol.
    #[cfg(test)]
    fn bincode_serialized_size(&self) -> usize {
        bincode::serialized_size(self)
            .map(usize::try_from)
            .unwrap()
            .unwrap()
    }

    // Returns true if all signatures verify.
    #[must_use]
    pub(crate) fn verify(&self) -> bool {
        match self {
            Self::PullRequest(_, caller) => caller.verify(),
            Self::PullResponse(_, data) => Self::verify_crds_values(data),
            Self::PushMessage(_, data) => Self::verify_crds_values(data),
            Self::PruneMessage(_, data) => data.verify(),
            Self::PingMessage(ping) => ping.verify(),
            Self::PongMessage(pong) => pong.verify(),
        }
    }
}

fn gossip_batch_verify_min_values() -> usize {
    static VALUE: OnceLock<usize> = OnceLock::new();
    *VALUE.get_or_init(|| {
        parse_gossip_batch_verify_min_values(
            std::env::var(GOSSIP_BATCH_VERIFY_MIN_VALUES_ENV).ok().as_deref(),
        )
    })
}

fn parse_gossip_batch_verify_min_values(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_GOSSIP_BATCH_VERIFY_MIN_VALUES)
}

impl PruneData {
    fn signable_data_without_prefix(&self) -> Cow<'_, [u8]> {
        #[derive(Serialize)]
        struct SignData<'a> {
            pubkey: &'a Pubkey,
            prunes: &'a [Pubkey],
            destination: &'a Pubkey,
            wallclock: u64,
        }
        let data = SignData {
            pubkey: &self.pubkey,
            prunes: &self.prunes,
            destination: &self.destination,
            wallclock: self.wallclock,
        };
        Cow::Owned(serialize(&data).expect("should serialize PruneData"))
    }

    #[cfg(test)]
    fn signable_data_with_prefix(&self) -> Cow<'_, [u8]> {
        #[derive(Serialize)]
        struct SignDataWithPrefix<'a> {
            prefix: &'a [u8],
            pubkey: &'a Pubkey,
            prunes: &'a [Pubkey],
            destination: &'a Pubkey,
            wallclock: u64,
        }
        let data = SignDataWithPrefix {
            prefix: PRUNE_DATA_PREFIX,
            pubkey: &self.pubkey,
            prunes: &self.prunes,
            destination: &self.destination,
            wallclock: self.wallclock,
        };
        Cow::Owned(serialize(&data).expect("should serialize PruneData"))
    }

    fn verify_data(&self, use_prefix: bool) -> bool {
        let mut buffer = ArrayVec::<u8, PACKET_DATA_SIZE>::new();
        let result = if !use_prefix {
            #[derive(Serialize)]
            struct SignData<'a> {
                pubkey: &'a Pubkey,
                prunes: &'a [Pubkey],
                destination: &'a Pubkey,
                wallclock: u64,
            }
            let data = SignData {
                pubkey: &self.pubkey,
                prunes: &self.prunes,
                destination: &self.destination,
                wallclock: self.wallclock,
            };
            bincode::serialize_into(&mut buffer, &data)
        } else {
            #[derive(Serialize)]
            struct SignDataWithPrefix<'a> {
                prefix: &'a [u8],
                pubkey: &'a Pubkey,
                prunes: &'a [Pubkey],
                destination: &'a Pubkey,
                wallclock: u64,
            }
            let data = SignDataWithPrefix {
                prefix: PRUNE_DATA_PREFIX,
                pubkey: &self.pubkey,
                prunes: &self.prunes,
                destination: &self.destination,
                wallclock: self.wallclock,
            };
            bincode::serialize_into(&mut buffer, &data)
        };
        result
            .map(|()| self.get_signature().verify(self.pubkey().as_ref(), &buffer))
            .unwrap_or(false)
    }
}

impl Sanitize for Protocol {
    fn sanitize(&self) -> Result<(), SanitizeError> {
        match self {
            Protocol::PullRequest(filter, val) => {
                filter.sanitize()?;
                // PullRequest is only allowed to have ContactInfo in its CrdsData
                match val.data() {
                    CrdsData::LegacyContactInfo(_) | CrdsData::ContactInfo(_) => val.sanitize(),
                    _ => Err(SanitizeError::InvalidValue),
                }
            }
            Protocol::PullResponse(_, val) => {
                // PullResponse is allowed to carry anything in its CrdsData, including deprecated Crds
                // such that a deprecated Crds does not get pulled and then rejected.
                val.sanitize()
            }
            Protocol::PushMessage(_, val) => {
                // PushMessage is allowed to carry anything in its CrdsData, including deprecated Crds
                // such that a deprecated Crds gets ingested instead of the node having to pull it from
                // other nodes that have inserted it into their Crds table
                val.sanitize()
            }
            Protocol::PruneMessage(from, val) => {
                if *from != val.pubkey {
                    Err(SanitizeError::InvalidValue)
                } else {
                    val.sanitize()
                }
            }
            Protocol::PingMessage(ping) => ping.sanitize(),
            Protocol::PongMessage(pong) => pong.sanitize(),
        }
    }
}

impl Sanitize for PruneData {
    fn sanitize(&self) -> Result<(), SanitizeError> {
        if self.wallclock >= MAX_WALLCLOCK {
            return Err(SanitizeError::ValueOutOfBounds);
        }
        Ok(())
    }
}

impl Signable for PruneData {
    fn pubkey(&self) -> Pubkey {
        self.pubkey
    }

    fn signable_data(&self) -> Cow<'_, [u8]> {
        // Continue to return signable data without a prefix until cluster has upgraded
        self.signable_data_without_prefix()
    }

    fn get_signature(&self) -> Signature {
        self.signature
    }

    fn set_signature(&mut self, signature: Signature) {
        self.signature = signature
    }

    // override Signable::verify default
    fn verify(&self) -> bool {
        // Try to verify PruneData with both prefixed and non-prefixed data
        self.verify_data(false) || self.verify_data(true)
    }
}

/// Splits an input feed of serializable data into chunks where the sum of
/// serialized size of values within each chunk is no larger than
/// max_chunk_size.
/// Note: some messages cannot be contained within that size so in the worst case this returns
/// N nested Vecs with 1 item each.
pub(crate) fn split_gossip_messages<T: Serialize + Debug>(
    max_chunk_size: usize,
    data_feed: impl IntoIterator<Item = T>,
) -> impl Iterator<Item = Vec<T>> {
    let mut data_feed = data_feed.into_iter().fuse();
    let mut buffer = vec![];
    let mut buffer_size = 0; // Serialized size of buffered values.
    std::iter::from_fn(move || loop {
        let Some(data) = data_feed.next() else {
            return (!buffer.is_empty()).then(|| std::mem::take(&mut buffer));
        };
        let data_size = match bincode::serialized_size(&data) {
            Ok(size) => size as usize,
            Err(err) => {
                error!("serialized_size failed: {err:?}");
                continue;
            }
        };
        if buffer_size + data_size <= max_chunk_size {
            buffer_size += data_size;
            buffer.push(data);
        } else if data_size <= max_chunk_size {
            buffer_size = data_size;
            return Some(std::mem::replace(&mut buffer, vec![data]));
        } else {
            error!("dropping data larger than the maximum chunk size {data:?}",);
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use {
        super::*,
        crate::{
            contact_info::ContactInfo,
            crds_data::{
                self, AccountsHashes, CrdsData, LowestSlot, SnapshotHashes, Vote as CrdsVote,
            },
        },
        rand::Rng,
        solana_clock::Slot,
        solana_hash::Hash,
        solana_keypair::{signable::Signable, Keypair},
        solana_perf::packet::Packet,
        solana_signer::Signer,
        solana_time_utils::timestamp,
        solana_transaction::Transaction,
        solana_vote_program::{vote_instruction, vote_state::Vote},
        std::{
            iter::repeat_with,
            net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
            sync::Arc,
        },
    };

    #[cfg(feature = "duplicate-shred-rocksdb")]
    use {
        crate::duplicate_shred::{self, tests::new_rand_shred, MAX_DUPLICATE_SHREDS},
        solana_ledger::shred::Shredder,
    };

    fn new_rand_socket_addr<R: Rng>(rng: &mut R) -> SocketAddr {
        let addr = if rng.gen_bool(0.5) {
            IpAddr::V4(Ipv4Addr::new(rng.gen(), rng.gen(), rng.gen(), rng.gen()))
        } else {
            IpAddr::V6(Ipv6Addr::new(
                rng.gen(),
                rng.gen(),
                rng.gen(),
                rng.gen(),
                rng.gen(),
                rng.gen(),
                rng.gen(),
                rng.gen(),
            ))
        };
        SocketAddr::new(addr, /*port=*/ rng.gen())
    }

    pub(crate) fn new_rand_remote_node<R>(rng: &mut R) -> (Keypair, SocketAddr)
    where
        R: Rng,
    {
        let keypair = Keypair::new();
        let socket = new_rand_socket_addr(rng);
        (keypair, socket)
    }

    fn new_rand_prune_data<R: Rng>(
        rng: &mut R,
        self_keypair: &Keypair,
        num_nodes: Option<usize>,
    ) -> PruneData {
        let wallclock = crds_data::new_rand_timestamp(rng);
        let num_nodes = num_nodes.unwrap_or_else(|| rng.gen_range(0..MAX_PRUNE_DATA_NODES + 1));
        let prunes = std::iter::repeat_with(Pubkey::new_unique)
            .take(num_nodes)
            .collect();
        let mut prune_data = PruneData {
            pubkey: self_keypair.pubkey(),
            prunes,
            signature: Signature::default(),
            destination: Pubkey::new_unique(),
            wallclock,
        };
        prune_data.sign(self_keypair);
        prune_data
    }

    #[test]
    fn test_max_accounts_hashes_with_push_messages() {
        let mut rng = rand::thread_rng();
        for _ in 0..256 {
            let accounts_hash = AccountsHashes::new_rand(&mut rng, None);
            let crds_value =
                CrdsValue::new(CrdsData::AccountsHashes(accounts_hash), &Keypair::new());
            let message = Protocol::PushMessage(Pubkey::new_unique(), vec![crds_value]);
            let socket = new_rand_socket_addr(&mut rng);
            assert!(Packet::from_data(Some(&socket), message).is_ok());
        }
    }

    #[test]
    fn test_max_accounts_hashes_with_pull_responses() {
        let mut rng = rand::thread_rng();
        for _ in 0..256 {
            let accounts_hash = AccountsHashes::new_rand(&mut rng, None);
            let crds_value =
                CrdsValue::new(CrdsData::AccountsHashes(accounts_hash), &Keypair::new());
            let response = Protocol::PullResponse(Pubkey::new_unique(), vec![crds_value]);
            let socket = new_rand_socket_addr(&mut rng);
            assert!(Packet::from_data(Some(&socket), response).is_ok());
        }
    }

    #[test]
    fn test_max_snapshot_hashes_with_push_messages() {
        let mut rng = rand::thread_rng();
        let snapshot_hashes = SnapshotHashes {
            from: Pubkey::new_unique(),
            full: (Slot::default(), Hash::default()),
            incremental: vec![(Slot::default(), Hash::default()); MAX_INCREMENTAL_SNAPSHOT_HASHES],
            wallclock: timestamp(),
        };
        let crds_value = CrdsValue::new(CrdsData::SnapshotHashes(snapshot_hashes), &Keypair::new());
        let message = Protocol::PushMessage(Pubkey::new_unique(), vec![crds_value]);
        let socket = new_rand_socket_addr(&mut rng);
        assert!(Packet::from_data(Some(&socket), message).is_ok());
    }

    #[test]
    fn test_max_snapshot_hashes_with_pull_responses() {
        let mut rng = rand::thread_rng();
        let snapshot_hashes = SnapshotHashes {
            from: Pubkey::new_unique(),
            full: (Slot::default(), Hash::default()),
            incremental: vec![(Slot::default(), Hash::default()); MAX_INCREMENTAL_SNAPSHOT_HASHES],
            wallclock: timestamp(),
        };
        let crds_value = CrdsValue::new(CrdsData::SnapshotHashes(snapshot_hashes), &Keypair::new());
        let response = Protocol::PullResponse(Pubkey::new_unique(), vec![crds_value]);
        let socket = new_rand_socket_addr(&mut rng);
        assert!(Packet::from_data(Some(&socket), response).is_ok());
    }

    #[test]
    fn test_max_prune_data_pubkeys() {
        let mut rng = rand::thread_rng();
        for _ in 0..64 {
            let self_keypair = Keypair::new();
            let prune_data =
                new_rand_prune_data(&mut rng, &self_keypair, Some(MAX_PRUNE_DATA_NODES));
            let prune_message = Protocol::PruneMessage(self_keypair.pubkey(), prune_data);
            let socket = new_rand_socket_addr(&mut rng);
            assert!(Packet::from_data(Some(&socket), prune_message).is_ok());
        }
        // Assert that MAX_PRUNE_DATA_NODES is highest possible.
        let self_keypair = Keypair::new();
        let prune_data =
            new_rand_prune_data(&mut rng, &self_keypair, Some(MAX_PRUNE_DATA_NODES + 1));
        let prune_message = Protocol::PruneMessage(self_keypair.pubkey(), prune_data);
        let socket = new_rand_socket_addr(&mut rng);
        assert!(Packet::from_data(Some(&socket), prune_message).is_err());
    }

    #[test]
    fn test_push_message_max_payload_size() {
        let header = Protocol::PushMessage(Pubkey::default(), Vec::default());
        assert_eq!(
            PUSH_MESSAGE_MAX_PAYLOAD_SIZE,
            PACKET_DATA_SIZE - header.bincode_serialized_size()
        );
    }

    #[test]
    fn test_pull_response_max_payload_size() {
        let header = Protocol::PullResponse(Pubkey::default(), Vec::default());
        assert_eq!(
            PULL_RESPONSE_MAX_PAYLOAD_SIZE,
            PACKET_DATA_SIZE - header.bincode_serialized_size()
        );
    }

    #[test]
    #[cfg(feature = "duplicate-shred-rocksdb")]
    fn test_duplicate_shred_max_payload_size() {
        let mut rng = rand::thread_rng();
        let leader = Arc::new(Keypair::new());
        let keypair = Keypair::new();
        let (slot, parent_slot, reference_tick, version) = (53084024, 53084023, 0, 0);
        let shredder = Shredder::new(slot, parent_slot, reference_tick, version).unwrap();
        let next_shred_index = rng.gen_range(0..32_000);
        let shred = new_rand_shred(&mut rng, next_shred_index, &shredder, &leader);
        let other_payload = {
            let other_shred = new_rand_shred(&mut rng, next_shred_index, &shredder, &leader);
            other_shred.into_payload()
        };
        let leader_schedule = |s| {
            if s == slot {
                Some(leader.pubkey())
            } else {
                None
            }
        };
        let chunks: Vec<_> = duplicate_shred::from_shred(
            shred,
            keypair.pubkey(),
            other_payload,
            Some(leader_schedule),
            timestamp(),
            DUPLICATE_SHRED_MAX_PAYLOAD_SIZE,
            version,
        )
        .unwrap()
        .collect();
        assert!(chunks.len() > 1);
        for chunk in chunks {
            let data = CrdsData::DuplicateShred(MAX_DUPLICATE_SHREDS - 1, chunk);
            let value = CrdsValue::new(data, &keypair);
            let pull_response = Protocol::PullResponse(keypair.pubkey(), vec![value.clone()]);
            assert!(pull_response.bincode_serialized_size() < PACKET_DATA_SIZE);
            let push_message = Protocol::PushMessage(keypair.pubkey(), vec![value.clone()]);
            assert!(push_message.bincode_serialized_size() < PACKET_DATA_SIZE);
        }
    }

    #[test]
    fn test_pull_response_min_serialized_size() {
        let mut rng = rand::thread_rng();
        for _ in 0..100 {
            let crds_values = vec![CrdsValue::new_rand(&mut rng, None)];
            let pull_response = Protocol::PullResponse(Pubkey::new_unique(), crds_values);
            let size = pull_response.bincode_serialized_size();
            assert!(
                PULL_RESPONSE_MIN_SERIALIZED_SIZE <= size,
                "pull-response serialized size: {size}"
            );
        }
    }

    #[test]
    fn test_split_messages_small() {
        let value = CrdsValue::new_unsigned(CrdsData::from(ContactInfo::default()));
        test_split_messages(value);
    }

    #[test]
    fn test_split_messages_large() {
        let value = CrdsValue::new_unsigned(CrdsData::LowestSlot(
            0,
            LowestSlot::new(Pubkey::default(), 0, 0),
        ));
        test_split_messages(value);
    }

    #[test]
    fn test_split_gossip_messages() {
        const NUM_CRDS_VALUES: usize = 2048;
        let mut rng = rand::thread_rng();
        let values: Vec<_> = repeat_with(|| CrdsValue::new_rand(&mut rng, None))
            .take(NUM_CRDS_VALUES)
            .collect();
        let splits: Vec<_> =
            split_gossip_messages(PUSH_MESSAGE_MAX_PAYLOAD_SIZE, values.clone()).collect();
        let self_pubkey = solana_pubkey::new_rand();
        assert!(splits.len() * 2 < NUM_CRDS_VALUES);
        // Assert that all messages are included in the splits.
        assert_eq!(NUM_CRDS_VALUES, splits.iter().map(Vec::len).sum::<usize>());
        splits
            .iter()
            .flat_map(|s| s.iter())
            .zip(values)
            .for_each(|(a, b)| assert_eq!(*a, b));
        let socket = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(rng.gen(), rng.gen(), rng.gen(), rng.gen()),
            rng.gen(),
        ));
        let header_size = PACKET_DATA_SIZE - PUSH_MESSAGE_MAX_PAYLOAD_SIZE;
        for values in splits {
            // Assert that sum of parts equals the whole.
            let size = header_size
                + values
                    .iter()
                    .map(CrdsValue::bincode_serialized_size)
                    .sum::<usize>();
            let message = Protocol::PushMessage(self_pubkey, values);
            assert_eq!(message.bincode_serialized_size(), size);
            // Assert that the message fits into a packet.
            assert!(Packet::from_data(Some(&socket), message).is_ok());
        }
    }

    #[test]
    fn test_split_gossip_messages_pull_response() {
        const NUM_CRDS_VALUES: usize = 2048;
        let mut rng = rand::thread_rng();
        let values: Vec<_> = repeat_with(|| CrdsValue::new_rand(&mut rng, None))
            .take(NUM_CRDS_VALUES)
            .collect();
        let splits: Vec<_> =
            split_gossip_messages(PULL_RESPONSE_MAX_PAYLOAD_SIZE, values.clone()).collect();
        let self_pubkey = solana_pubkey::new_rand();
        assert!(splits.len() * 2 < NUM_CRDS_VALUES);
        // Assert that all messages are included in the splits.
        assert_eq!(NUM_CRDS_VALUES, splits.iter().map(Vec::len).sum::<usize>());
        splits
            .iter()
            .flat_map(|s| s.iter())
            .zip(values)
            .for_each(|(a, b)| assert_eq!(*a, b));
        let socket = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(rng.gen(), rng.gen(), rng.gen(), rng.gen()),
            rng.gen(),
        ));
        // check message fits into PullResponse
        let header_size = PACKET_DATA_SIZE - PULL_RESPONSE_MAX_PAYLOAD_SIZE;
        for values in splits {
            // Assert that sum of parts equals the whole.
            let size = header_size
                + values
                    .iter()
                    .map(CrdsValue::bincode_serialized_size)
                    .sum::<usize>();
            let message = Protocol::PullResponse(self_pubkey, values);
            assert_eq!(message.bincode_serialized_size(), size);
            // Assert that the message fits into a packet.
            assert!(Packet::from_data(Some(&socket), message).is_ok());
        }
    }

    #[test]
    fn test_split_messages_packet_size() {
        // Test that if a value is smaller than payload size but too large to be wrapped in a vec
        // that it is still dropped
        let mut value = CrdsValue::new_unsigned(CrdsData::AccountsHashes(AccountsHashes {
            from: Pubkey::default(),
            hashes: vec![],
            wallclock: 0,
        }));

        let mut i = 0;
        while value.bincode_serialized_size() < PUSH_MESSAGE_MAX_PAYLOAD_SIZE {
            value = CrdsValue::new_unsigned(CrdsData::AccountsHashes(AccountsHashes {
                from: Pubkey::default(),
                hashes: vec![(0, Hash::default()); i],
                wallclock: 0,
            }));
            i += 1;
        }
        let split: Vec<_> =
            split_gossip_messages(PUSH_MESSAGE_MAX_PAYLOAD_SIZE, vec![value]).collect();
        assert_eq!(split.len(), 0);
    }

    fn test_split_messages(value: CrdsValue) {
        const NUM_VALUES: usize = 30;
        let value_size = value.bincode_serialized_size();
        let num_values_per_payload = (PUSH_MESSAGE_MAX_PAYLOAD_SIZE / value_size).max(1);

        // Expected len is the ceiling of the division
        let expected_len = NUM_VALUES.div_ceil(num_values_per_payload);
        let msgs = vec![value; NUM_VALUES];

        assert!(split_gossip_messages(PUSH_MESSAGE_MAX_PAYLOAD_SIZE, msgs).count() <= expected_len);
    }

    #[test]
    fn test_protocol_sanitize() {
        let pd = PruneData {
            wallclock: MAX_WALLCLOCK,
            ..PruneData::default()
        };
        let msg = Protocol::PruneMessage(Pubkey::default(), pd);
        assert_eq!(msg.sanitize(), Err(SanitizeError::ValueOutOfBounds));
    }

    #[test]
    fn test_protocol_prune_message_sanitize() {
        let keypair = Keypair::new();
        let mut prune_data = PruneData {
            pubkey: keypair.pubkey(),
            prunes: vec![],
            signature: Signature::default(),
            destination: Pubkey::new_unique(),
            wallclock: timestamp(),
        };
        prune_data.sign(&keypair);
        let prune_message = Protocol::PruneMessage(keypair.pubkey(), prune_data.clone());
        assert_eq!(prune_message.sanitize(), Ok(()));
        let prune_message = Protocol::PruneMessage(Pubkey::new_unique(), prune_data);
        assert_eq!(prune_message.sanitize(), Err(SanitizeError::InvalidValue));
    }

    #[test]
    fn test_vote_size() {
        let slots = vec![1; 32];
        let vote = Vote::new(slots, Hash::default());
        let keypair = Arc::new(Keypair::new());

        // Create the biggest possible vote transaction
        let vote_ix = vote_instruction::vote_switch(
            &keypair.pubkey(),
            &keypair.pubkey(),
            vote,
            Hash::default(),
        );
        let mut vote_tx = Transaction::new_with_payer(&[vote_ix], Some(&keypair.pubkey()));

        vote_tx.partial_sign(&[keypair.as_ref()], Hash::default());
        vote_tx.partial_sign(&[keypair.as_ref()], Hash::default());

        let vote = CrdsVote::new(
            keypair.pubkey(),
            vote_tx,
            0, // wallclock
        )
        .unwrap();
        let vote = CrdsValue::new(CrdsData::Vote(1, vote), &Keypair::new());
        assert!(vote.bincode_serialized_size() <= PUSH_MESSAGE_MAX_PAYLOAD_SIZE);
    }

    #[test]
    fn test_prune_data_sign_and_verify_without_prefix() {
        let mut rng = rand::thread_rng();
        let keypair = Keypair::new();
        let mut prune_data = new_rand_prune_data(&mut rng, &keypair, Some(3));

        prune_data.sign(&keypair);

        let is_valid = prune_data.verify();
        assert!(is_valid, "Signature should be valid without prefix");
    }

    #[test]
    fn test_prune_data_sign_and_verify_with_prefix() {
        let mut rng = rand::thread_rng();
        let keypair = Keypair::new();
        let mut prune_data = new_rand_prune_data(&mut rng, &keypair, Some(3));

        // Manually set the signature with prefixed data
        let prefixed_data = prune_data.signable_data_with_prefix();
        let signature_with_prefix = keypair.sign_message(prefixed_data.as_ref());
        prune_data.set_signature(signature_with_prefix);

        let is_valid = prune_data.verify();
        assert!(is_valid, "Signature should be valid with prefix");
    }

    #[test]
    fn test_prune_data_verify_with_and_without_prefix() {
        let mut rng = rand::thread_rng();
        let keypair = Keypair::new();
        let mut prune_data = new_rand_prune_data(&mut rng, &keypair, Some(3));

        // Sign with non-prefixed data
        prune_data.sign(&keypair);
        let is_valid_non_prefixed = prune_data.verify();
        assert!(
            is_valid_non_prefixed,
            "Signature should be valid without prefix"
        );

        // Save the original non-prefixed, serialized data for last check
        let non_prefixed_data = prune_data.signable_data_without_prefix().into_owned();

        // Manually set the signature with prefixed, serialized data
        let prefixed_data = prune_data.signable_data_with_prefix();
        let signature_with_prefix = keypair.sign_message(prefixed_data.as_ref());
        prune_data.set_signature(signature_with_prefix);

        let is_valid_prefixed = prune_data.verify();
        assert!(is_valid_prefixed, "Signature should be valid with prefix");

        // Ensure prefixed and non-prefixed serialized data are different
        let prefixed_data = prune_data.signable_data_with_prefix();
        assert_ne!(
            prefixed_data.as_ref(),
            non_prefixed_data.as_slice(),
            "Prefixed and non-prefixed serialized data should be different"
        );
    }

    #[test]
    fn test_verify_crds_values_batch_falls_back_to_strict_on_invalid_signature() {
        let keypair = Keypair::new();
        let mut values: Vec<_> = (0..DEFAULT_GOSSIP_BATCH_VERIFY_MIN_VALUES)
            .map(|index| {
                CrdsValue::new(
                    CrdsData::LowestSlot(0, LowestSlot::new(keypair.pubkey(), index as u64, 0)),
                    &keypair,
                )
            })
            .collect();
        let mut signature = *values[0].get_signature().as_array();
        signature[0] ^= 1;
        values[0].set_signature(Signature::from(signature));
        let message = Protocol::PushMessage(keypair.pubkey(), values);
        assert!(!message.verify());
    }

    #[test]
    fn test_verify_crds_values_rejects_weak_pubkeys_before_batch_verify() {
        let mut weak_pubkey_bytes = [0u8; 32];
        weak_pubkey_bytes[0] = 1;
        let weak_pubkey = Pubkey::new_from_array(weak_pubkey_bytes);
        let values: Vec<_> = (0..DEFAULT_GOSSIP_BATCH_VERIFY_MIN_VALUES)
            .map(|index| {
                CrdsValue::new_unsigned(CrdsData::LowestSlot(
                    0,
                    LowestSlot::new(weak_pubkey, index as u64, timestamp()),
                ))
            })
            .collect();
        let message = Protocol::PullResponse(Pubkey::new_unique(), values);
        assert!(!message.verify());
    }

    #[test]
    fn test_parse_gossip_batch_verify_min_values_defaults_on_missing() {
        assert_eq!(
            parse_gossip_batch_verify_min_values(None),
            DEFAULT_GOSSIP_BATCH_VERIFY_MIN_VALUES
        );
    }

    #[test]
    fn test_parse_gossip_batch_verify_min_values_rejects_zero_and_invalid() {
        assert_eq!(
            parse_gossip_batch_verify_min_values(Some("0")),
            DEFAULT_GOSSIP_BATCH_VERIFY_MIN_VALUES
        );
        assert_eq!(
            parse_gossip_batch_verify_min_values(Some("invalid")),
            DEFAULT_GOSSIP_BATCH_VERIFY_MIN_VALUES
        );
    }

    #[test]
    fn test_parse_gossip_batch_verify_min_values_accepts_positive_override() {
        assert_eq!(parse_gossip_batch_verify_min_values(Some("4")), 4);
        assert_eq!(parse_gossip_batch_verify_min_values(Some("16")), 16);
    }
}
