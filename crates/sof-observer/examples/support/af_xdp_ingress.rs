use std::{
    ffi::CString,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sof::{
    ingest::{RawPacketBatch, RawPacketIngress},
    runtime::KernelBypassIngressSender,
};
use xdp::{
    RingConfigBuilder, Umem,
    slab::{HeapSlab, Slab},
    socket::{PollTimeout, XdpSocket, XdpSocketBuilder},
    umem::{FrameSize, UmemCfgBuilder},
};

pub(crate) type ExampleError = Box<dyn std::error::Error + Send + Sync>;

pub(crate) const DEFAULT_INTERFACE: &str = "enp17s0";
pub(crate) const DEFAULT_QUEUE_ID: u32 = 0;
pub(crate) const DEFAULT_BATCH_SIZE: usize = 64;
pub(crate) const DEFAULT_UMEM_FRAME_COUNT: u32 = 4_096;
pub(crate) const DEFAULT_RING_DEPTH: u32 = 2_048;
pub(crate) const DEFAULT_POLL_TIMEOUT_MS: u64 = 100;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct PortFilter {
    pub(crate) range_start: u16,
    pub(crate) range_end: u16,
    pub(crate) extra_port: u16,
}

impl PortFilter {
    pub(crate) const fn default_sol() -> Self {
        Self {
            range_start: 12_000,
            range_end: 12_100,
            extra_port: 8_001,
        }
    }

    pub(crate) const fn allows(self, port: u16) -> bool {
        (port >= self.range_start && port <= self.range_end) || port == self.extra_port
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct AfXdpConfig {
    pub(crate) interface: String,
    pub(crate) queue_id: u32,
    pub(crate) batch_size: usize,
    pub(crate) umem_frame_count: u32,
    pub(crate) ring_depth: u32,
    pub(crate) poll_timeout: Duration,
    pub(crate) filter: PortFilter,
}

pub(crate) struct AfXdpSocketState {
    pub(crate) socket: XdpSocket,
    pub(crate) rings: xdp::WakableRings,
    pub(crate) umem: Umem,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct AfXdpProducerStats {
    pub(crate) frames_seen: u64,
    pub(crate) udp_frames_forwarded: u64,
    pub(crate) filtered_frames: u64,
    pub(crate) parse_error_frames: u64,
    pub(crate) batches_sent: u64,
    pub(crate) bytes_forwarded: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum FrameParseOutcome {
    ParseError,
}

pub(crate) fn read_af_xdp_config() -> AfXdpConfig {
    AfXdpConfig {
        interface: std::env::var("SOF_AF_XDP_IFACE")
            .unwrap_or_else(|_| DEFAULT_INTERFACE.to_owned()),
        queue_id: read_env_u32("SOF_AF_XDP_QUEUE_ID", DEFAULT_QUEUE_ID),
        batch_size: read_env_usize("SOF_AF_XDP_BATCH_SIZE", DEFAULT_BATCH_SIZE),
        umem_frame_count: read_env_u32("SOF_AF_XDP_UMEM_FRAMES", DEFAULT_UMEM_FRAME_COUNT),
        ring_depth: read_env_u32("SOF_AF_XDP_RING_DEPTH", DEFAULT_RING_DEPTH),
        poll_timeout: Duration::from_millis(read_env_u64(
            "SOF_AF_XDP_POLL_TIMEOUT_MS",
            DEFAULT_POLL_TIMEOUT_MS,
        )),
        filter: PortFilter::default_sol(),
    }
}

pub(crate) fn run_af_xdp_producer_until(
    tx: &KernelBypassIngressSender,
    config: &AfXdpConfig,
    shutdown: Arc<AtomicBool>,
) -> Result<AfXdpProducerStats, ExampleError> {
    let mut state = build_af_xdp_socket(config)?;
    let mut slab = HeapSlab::with_capacity(usize::try_from(config.ring_depth).unwrap_or(0));
    let mut batch = RawPacketBatch::with_capacity(config.batch_size);
    let mut stats = AfXdpProducerStats::default();

    while !shutdown.load(Ordering::Relaxed) {
        drop(
            state
                .socket
                .poll_read(PollTimeout::new(Some(config.poll_timeout))),
        );

        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let Some(rx_ring) = state.rings.rx_ring.as_mut() else {
            return Err(io::Error::other("AF_XDP socket has no RX ring configured").into());
        };
        // SAFETY: `rx_ring` is paired with `state.umem` from the same socket setup,
        // and `slab` is the destination scratch storage expected by the xdp crate.
        let received = unsafe { rx_ring.recv(&state.umem, &mut slab) };
        if received == 0 {
            continue;
        }

        for _ in 0..received {
            let Some(frame) = slab.pop_back() else {
                break;
            };
            stats.frames_seen = stats.frames_seen.saturating_add(1);
            match parse_udp_payload_to_raw_packet(&frame, config.filter) {
                Ok(Some((source, payload))) => {
                    stats.udp_frames_forwarded = stats.udp_frames_forwarded.saturating_add(1);
                    stats.bytes_forwarded = stats
                        .bytes_forwarded
                        .saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
                    batch.push_packet_bytes(source, RawPacketIngress::Udp, payload)?;
                    if batch.len() >= config.batch_size {
                        if !tx.send_batch(std::mem::take(&mut batch), false) {
                            state.umem.free_packet(frame);
                            return Ok(stats);
                        }
                        batch = RawPacketBatch::with_capacity(config.batch_size);
                        stats.batches_sent = stats.batches_sent.saturating_add(1);
                    }
                }
                Ok(None) => {
                    stats.filtered_frames = stats.filtered_frames.saturating_add(1);
                }
                Err(FrameParseOutcome::ParseError) => {
                    stats.parse_error_frames = stats.parse_error_frames.saturating_add(1);
                }
            }
            state.umem.free_packet(frame);
        }

        // SAFETY: Returned RX buffers are recycled into the matching fill ring for the
        // same `umem`, bounded by the number of descriptors just received.
        unsafe {
            state
                .rings
                .fill_ring
                .enqueue(&mut state.umem, received, true)?
        };
    }

    if !batch.is_empty() && tx.send_batch(batch, false) {
        stats.batches_sent = stats.batches_sent.saturating_add(1);
    }

    Ok(stats)
}

pub(crate) fn build_af_xdp_socket(config: &AfXdpConfig) -> Result<AfXdpSocketState, ExampleError> {
    let ifname = CString::new(config.interface.clone())?;
    let nic = xdp::nic::NicIndex::lookup_by_name(&ifname)?.ok_or_else(|| {
        io::Error::other(format!(
            "network interface `{}` not found",
            config.interface
        ))
    })?;

    let mut builder = XdpSocketBuilder::new()?;
    let umem_cfg = UmemCfgBuilder {
        frame_size: FrameSize::TwoK,
        frame_count: config.umem_frame_count,
        ..Default::default()
    }
    .build()?;
    let mut umem = Umem::map(umem_cfg)?;
    let ring_cfg = RingConfigBuilder {
        rx_count: config.ring_depth,
        tx_count: 0,
        fill_count: config.ring_depth,
        completion_count: config.ring_depth,
    }
    .build()?;
    let (mut rings, bind_flags) = builder.build_wakable_rings(&umem, ring_cfg)?;
    let socket = builder.bind(nic, config.queue_id, bind_flags)?;

    // SAFETY: `umem` and `fill_ring` were created together by `build_wakable_rings`,
    // and the requested descriptor count is bounded by the configured ring depth.
    let _fill_queued = unsafe {
        rings.fill_ring.enqueue(
            &mut umem,
            usize::try_from(config.ring_depth).unwrap_or(0),
            true,
        )?
    };

    Ok(AfXdpSocketState {
        socket,
        rings,
        umem,
    })
}

fn parse_udp_payload_to_raw_packet(
    frame: &[u8],
    filter: PortFilter,
) -> Result<Option<(SocketAddr, &[u8])>, FrameParseOutcome> {
    const ETH_LEN: usize = 14;
    const ETH_TYPE_OFFSET: usize = 12;
    const ETH_P_IPV4: u16 = 0x0800;
    const IP_PROTO_UDP: u8 = 17;
    if frame.len() < ETH_LEN + 20 {
        return Err(FrameParseOutcome::ParseError);
    }

    if read_u16_be(frame, ETH_TYPE_OFFSET)? != ETH_P_IPV4 {
        return Ok(None);
    }

    let ip_offset = ETH_LEN;
    let version_ihl = read_u8(frame, ip_offset)?;
    if version_ihl >> 4 != 4 {
        return Ok(None);
    }
    let ihl = usize::from(version_ihl & 0x0F).checked_mul(4).unwrap_or(0);
    let udp_header_end = checked_add(checked_add(ip_offset, ihl)?, 8)?;
    if ihl < 20 || frame.len() < udp_header_end {
        return Err(FrameParseOutcome::ParseError);
    }
    if read_u8(frame, checked_add(ip_offset, 9)?)? != IP_PROTO_UDP {
        return Ok(None);
    }
    let frag_field = read_u16_be(frame, checked_add(ip_offset, 6)?)?;
    if (frag_field & 0x1FFF) != 0 {
        return Ok(None);
    }

    let udp_offset = checked_add(ip_offset, ihl)?;
    let src_port = read_u16_be(frame, udp_offset)?;
    let dst_port = read_u16_be(frame, checked_add(udp_offset, 2)?)?;
    if !filter.allows(dst_port) {
        return Ok(None);
    }
    let udp_len = usize::from(read_u16_be(frame, checked_add(udp_offset, 4)?)?);
    if udp_len < 8 {
        return Err(FrameParseOutcome::ParseError);
    }
    let payload_start = checked_add(udp_offset, 8)?;
    let payload_end = checked_add(payload_start, udp_len.saturating_sub(8))?;
    if payload_end > frame.len() {
        return Err(FrameParseOutcome::ParseError);
    }

    let source = SocketAddr::new(
        IpAddr::V4(read_ipv4_addr(frame, checked_add(ip_offset, 12)?)?),
        src_port,
    );
    Ok(Some((
        source,
        frame
            .get(payload_start..payload_end)
            .ok_or(FrameParseOutcome::ParseError)?,
    )))
}

fn checked_add(lhs: usize, rhs: usize) -> Result<usize, FrameParseOutcome> {
    lhs.checked_add(rhs).ok_or(FrameParseOutcome::ParseError)
}

fn read_u8(frame: &[u8], offset: usize) -> Result<u8, FrameParseOutcome> {
    frame
        .get(offset)
        .copied()
        .ok_or(FrameParseOutcome::ParseError)
}

fn read_u16_be(frame: &[u8], offset: usize) -> Result<u16, FrameParseOutcome> {
    let end = checked_add(offset, 2)?;
    let bytes: [u8; 2] = frame
        .get(offset..end)
        .ok_or(FrameParseOutcome::ParseError)?
        .try_into()
        .map_err(|_slice_error| FrameParseOutcome::ParseError)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_ipv4_addr(frame: &[u8], offset: usize) -> Result<Ipv4Addr, FrameParseOutcome> {
    let end = checked_add(offset, 4)?;
    let octets: [u8; 4] = frame
        .get(offset..end)
        .ok_or(FrameParseOutcome::ParseError)?
        .try_into()
        .map_err(|_slice_error| FrameParseOutcome::ParseError)?;
    Ok(Ipv4Addr::from(octets))
}

fn read_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn read_env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn read_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
