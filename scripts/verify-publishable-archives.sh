#!/usr/bin/env bash
set -euo pipefail

crates=("sof-gossip-tuning" "sof-solana-gossip" "sof" "sof-tx")
package_dir="target/package"
verify_root="$(mktemp -d)"
cargo_home_root="$(mktemp -d)"
verify_cargo_home="$(mktemp -d)"
trap 'rm -rf "${verify_root}" "${cargo_home_root}" "${verify_cargo_home}"' EXIT

cat > "${cargo_home_root}/config.toml" <<EOF
[patch.crates-io]
sof-gossip-tuning = { path = "$(pwd)/crates/sof-gossip-tuning" }
sof-solana-gossip = { path = "$(pwd)/crates/sof-solana-gossip" }
sof = { path = "$(pwd)/crates/sof-observer" }
EOF

export CARGO_HOME="${cargo_home_root}"

version_for() {
  if [[ "$1" == "sof-solana-gossip" ]]; then
    cargo pkgid --manifest-path "crates/sof-solana-gossip/Cargo.toml" | awk -F'[#@]' '{print $NF}'
  else
    cargo pkgid -p "$1" | awk -F'[#@]' '{print $NF}'
  fi
}

package_crate() {
  local crate="$1"
  local verify_flag="$2"
  local allow_dirty_flag=()
  if [[ "${VERIFY_ARCHIVES_ALLOW_DIRTY:-0}" == "1" ]]; then
    allow_dirty_flag=(--allow-dirty)
  elif [[ "${crate}" == "sof-solana-gossip" ]]; then
    # Cargo may refresh the vendored fork's standalone lockfile metadata during
    # packaging even when the committed contents are already correct. Treat that
    # as a packaging-time implementation detail rather than a release blocker.
    allow_dirty_flag=(--allow-dirty)
  fi
  echo "== packaging ${crate} (${verify_flag}) =="
  if [[ "${crate}" == "sof-solana-gossip" ]]; then
    cargo package --manifest-path "crates/sof-solana-gossip/Cargo.toml" --target-dir "target" "${allow_dirty_flag[@]}" ${verify_flag}
  else
    cargo package -p "${crate}" --locked "${allow_dirty_flag[@]}" ${verify_flag}
  fi
}

extract_crate() {
  local crate="$1"
  local version
  version="$(version_for "${crate}")"
  local archive="${package_dir}/${crate}-${version}.crate"
  echo "== extracting ${archive} =="
  tar -xzf "${archive}" -C "${verify_root}"
}

package_crate "sof-gossip-tuning" ""
package_crate "sof-solana-gossip" "--no-verify"
package_crate "sof" "--no-verify"
package_crate "sof-tx" "--no-verify"

for crate in "${crates[@]}"; do
  extract_crate "${crate}"
done

sof_gossip_tuning_version="$(version_for "sof-gossip-tuning")"
sof_solana_gossip_version="$(version_for "sof-solana-gossip")"
sof_version="$(version_for "sof")"
sof_tx_version="$(version_for "sof-tx")"

cat > "${verify_root}/Cargo.toml" <<EOF
[workspace]
resolver = "3"
members = [
  "sof-gossip-tuning-${sof_gossip_tuning_version}",
  "sof-${sof_version}",
  "sof-tx-${sof_tx_version}",
]

[patch.crates-io]
sof-gossip-tuning = { path = "sof-gossip-tuning-${sof_gossip_tuning_version}" }
sof-solana-gossip = { path = "sof-solana-gossip-${sof_solana_gossip_version}" }
sof = { path = "sof-${sof_version}" }
EOF

echo "== verifying packaged workspace =="
env CARGO_HOME="${verify_cargo_home}" cargo check \
  --manifest-path "${verify_root}/Cargo.toml" \
  --workspace
