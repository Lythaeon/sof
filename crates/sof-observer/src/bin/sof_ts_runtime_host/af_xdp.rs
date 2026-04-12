//! AF_XDP packet producer used by the TypeScript runtime host on Linux.

use std::{
    ffi::CString,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicBool, Ordering},
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

/// Error type returned by the AF_XDP helper module.
pub(super) type AfXdpError = Box<dyn std::error::Error + Send + Sync>;

/// AF_XDP socket and batching settings for direct raw-shred ingest.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct AfXdpConfig {
    /// Network interface name used for the XDP bind.
    pub(super) interface: String,
    /// Receive queue id bound on the target interface.
    pub(super) queue_id: u32,
    /// Maximum number of packets forwarded in one batch.
    pub(super) batch_size: usize,
    /// Number of UMEM frames allocated for the socket.
    pub(super) umem_frame_count: u32,
    /// RX/fill/completion ring depth.
    pub(super) ring_depth: u32,
    /// Poll timeout used while waiting for packets.
    pub(super) poll_timeout: Duration,
    /// Destination UDP port filter applied before forwarding.
    pub(super) filter: PortFilter,
}

/// UDP destination ports accepted by the AF_XDP producer.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct PortFilter {
    /// Inclusive lower bound of the accepted port range.
    pub(super) range_start: u16,
    /// Inclusive upper bound of the accepted port range.
    pub(super) range_end: u16,
    /// One additional accepted port outside the main range.
    pub(super) extra_port: u16,
}

impl PortFilter {
    /// Returns the default Solana TPU and TVU destination port filter.
    pub(super) const fn default_sol() -> Self {
        Self {
            range_start: 12_000,
            range_end: 12_100,
            extra_port: 8_001,
        }
    }

    /// Returns whether one UDP destination port should be forwarded.
    const fn allows(self, port: u16) -> bool {
        (port >= self.range_start && port <= self.range_end) || port == self.extra_port
    }
}

/// Live AF_XDP socket state used while receiving packets.
struct AfXdpSocketState {
    /// Bound XDP socket.
    socket: XdpSocket,
    /// Wakeable RX/fill/completion rings paired with the socket.
    rings: xdp::WakableRings,
    /// Shared packet memory region used by the rings.
    umem: Umem,
}

/// Internal packet parse outcome while decoding one Ethernet frame.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum FrameParseOutcome {
    /// The frame was malformed for the expected IPv4 UDP layout.
    ParseError,
}

/// Runs the AF_XDP receive loop until shutdown is requested.
pub(super) fn run_af_xdp_producer_until(
    tx: &KernelBypassIngressSender,
    config: &AfXdpConfig,
    shutdown: &AtomicBool,
) -> Result<(), AfXdpError> {
    let mut state = build_af_xdp_socket(config)?;
    let mut slab = HeapSlab::with_capacity(usize::try_from(config.ring_depth).unwrap_or(0));
    let mut batch = RawPacketBatch::with_capacity(config.batch_size);

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
            match parse_udp_payload_to_raw_packet(&frame, config.filter) {
                Ok(Some((source, payload))) => {
                    batch.push_packet_bytes(source, RawPacketIngress::Udp, payload)?;
                    if batch.len() >= config.batch_size {
                        if !tx.send_batch(std::mem::take(&mut batch), false) {
                            state.umem.free_packet(frame);
                            return Ok(());
                        }
                        batch = RawPacketBatch::with_capacity(config.batch_size);
                    }
                }
                Ok(None) | Err(FrameParseOutcome::ParseError) => {}
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

    if !batch.is_empty() {
        let _sent = tx.send_batch(batch, false);
    }

    Ok(())
}

/// Creates and primes one AF_XDP socket using the requested config.
fn build_af_xdp_socket(config: &AfXdpConfig) -> Result<AfXdpSocketState, AfXdpError> {
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

/// Parses one Ethernet frame into a raw UDP payload and source address.
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

/// Adds two byte offsets while rejecting overflow.
fn checked_add(lhs: usize, rhs: usize) -> Result<usize, FrameParseOutcome> {
    lhs.checked_add(rhs).ok_or(FrameParseOutcome::ParseError)
}

/// Reads one byte from the frame at the requested offset.
fn read_u8(frame: &[u8], offset: usize) -> Result<u8, FrameParseOutcome> {
    frame
        .get(offset)
        .copied()
        .ok_or(FrameParseOutcome::ParseError)
}

/// Reads one big-endian `u16` from the frame at the requested offset.
fn read_u16_be(frame: &[u8], offset: usize) -> Result<u16, FrameParseOutcome> {
    let end = checked_add(offset, 2)?;
    let bytes: [u8; 2] = frame
        .get(offset..end)
        .ok_or(FrameParseOutcome::ParseError)?
        .try_into()
        .map_err(|_slice_error| FrameParseOutcome::ParseError)?;
    Ok(u16::from_be_bytes(bytes))
}

/// Reads one IPv4 address from the frame at the requested offset.
fn read_ipv4_addr(frame: &[u8], offset: usize) -> Result<Ipv4Addr, FrameParseOutcome> {
    let end = checked_add(offset, 4)?;
    let octets: [u8; 4] = frame
        .get(offset..end)
        .ok_or(FrameParseOutcome::ParseError)?
        .try_into()
        .map_err(|_slice_error| FrameParseOutcome::ParseError)?;
    Ok(Ipv4Addr::from(octets))
}
