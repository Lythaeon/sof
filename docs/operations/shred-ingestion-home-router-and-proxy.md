# Live Shred Ingestion: Home Router and Proxy Paths

## Goal
Receive live Solana shreds into `sof-observer` and see non-zero ingest telemetry:
- `packets > 0`
- `data > 0` and/or `code > 0`
- `latest_shred_slot` near current `getSlot`

## Recommended Path (Proxy/Tunnel First)
Use this when running from home NAT/firewall. It avoids relying on turbine selecting your residential public endpoint.

### 1. Edge host (public VPS): run gossip receiver
Run `sof-observer` on a public host where UDP gossip/TVU is reachable:

```bash
SOF_GOSSIP_ENTRYPOINT=85.195.118.195:8001 \
SOF_PORT_RANGE=12000-12100 \
RUST_LOG=info \
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Expected:
- gossip stats show many nodes (`num_nodes` in the hundreds)

### 2. Home host (behind router/firewall): run local UDP sink
Run SOF as a plain local UDP listener:

```bash
SOF_BIND=127.0.0.1:20001 RUST_LOG=info cargo run --release -p sof --example observer_runtime
```

Expected telemetry every ~5s:
- `ingest telemetry packets=... data=... code=...`

### 3. Feed shreds from an upstream proxy/relay
Point your proxy to send UDP shreds to `127.0.0.1:20001` (or your chosen bind address).

For Jito-style proxy setups, configure destination to this listener and keep source connectivity outbound from your machine/VPS. You do not need direct inbound internet traffic to your home host for this mode.

### 4. Validate against live slot
In another shell:

```bash
curl -s https://api.mainnet-beta.solana.com \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}'
```

Compare RPC slot with `latest_shred_slot` from SOF logs.

## Direct Turbine Path (Home Router Port Forwarding)
Use only if you want gossip bootstrap and direct TVU receipts.

### 1. Router forwarding
Forward UDP range to your SOF host, for example:
- `12000-12100/udp` -> `<LAN_HOST_IP>:12000-12100`

### 2. Open local firewall
Allow the same UDP range on the host.

### 3. Run SOF in gossip bootstrap mode

```bash
SOF_GOSSIP_ENTRYPOINT=85.195.118.195:8001 \
SOF_PORT_RANGE=12000-12100 \
RUST_LOG=info \
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Notes:
- `SOF_SHRED_VERSION` is optional; leave it unset unless you intentionally override cluster autodetection.
- `SOF_GOSSIP_ENTRYPOINT` mode requires the `gossip-bootstrap` feature.
- If `tvu_peers=0` and telemetry stays zero, your endpoint is still not selected/reachable for turbine.
- `SOF_REPAIR_OUTSTANDING_TIMEOUT_MS` controls how long a sent repair request
  stays deduplicated in-flight before retry (default `150`).
- Stream-first repair recommendations for home/router mode:
  - keep defaults first (`SOF_REPAIR_MAX_REQUESTS_PER_TICK=4`, `SOF_REPAIR_PER_SLOT_CAP=16`);
  - if repairs dominate traffic, increase `SOF_REPAIR_MIN_SLOT_LAG` (for example `8`);
  - avoid large request-rate overrides because they can increase network load without improving live stream quality.

## Tunnel Alternative
Run a small relay on a public VPS and tunnel UDP into home:
- VPS receives public shred feed.
- VPS forwards UDP over WireGuard/Tailscale tunnel to `SOF_BIND` endpoint.
- SOF stays in simple listener mode (`SOF_BIND`), no gossip required.

This is usually more stable than exposing a home router directly.

Notes:
- Cloudflare Tunnel is TCP/HTTP focused and does not forward raw UDP datagrams for this use case.
- WireGuard or Tailscale between edge and home works well if you prefer encrypted private transport.

## Quick Troubleshooting
- `packets=0` forever:
  - verify sender is actually targeting your `SOF_BIND` endpoint.
  - `tcpdump -ni any udp port <port>` should show incoming datagrams.
- parse errors rising fast:
  - sender is not forwarding raw shreds; inspect packet source/payload.
- gossip mode with many nodes but no shreds:
  - common behind NAT even with gossip traffic present.
  - switch to proxy/tunnel feed.
