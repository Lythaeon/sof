#!/usr/bin/env bash
set -euo pipefail

violations=0

has_rg() {
  command -v rg >/dev/null 2>&1
}

search_rust_files() {
  local output_file="$1"
  local pattern="$2"
  shift 2

  if has_rg; then
    rg -n "${pattern}" "$@" >"${output_file}"
    return
  fi

  local grep_pattern="${pattern//\\b/}"
  find "$@" -type f -name '*.rs' ! -name 'tests.rs' -print0 \
    | xargs -0 grep -nE "${grep_pattern}" >"${output_file}"
}

search_mod_rs() {
  local output_file="$1"
  local pattern="$2"
  local root="$3"

  if has_rg; then
    rg -n --glob '**/mod.rs' "${pattern}" "${root}" >"${output_file}"
    return
  fi

  find "${root}" -type f -name 'mod.rs' -print0 | xargs -0 grep -nE "${pattern}" >"${output_file}"
}

check_forbidden_import() {
  local src_slice="$1"
  local forbidden_target="$2"
  local match_file="/tmp/sof-arch-check-${src_slice}-${forbidden_target}.log"

  if search_rust_files \
    "${match_file}" \
    "crate::${forbidden_target}" \
    "crates/sof-observer/src/${src_slice}"; then
    echo "ARD boundary violation: '${src_slice}' must not import '${forbidden_target}'"
    cat "${match_file}"
    violations=1
  fi
}

# ARD-0003/0007: slices are isolated; infra (app/runtime) composes them.
check_forbidden_import ingest shred
check_forbidden_import ingest reassembly
check_forbidden_import ingest app
check_forbidden_import shred ingest
check_forbidden_import shred reassembly
check_forbidden_import reassembly ingest
check_forbidden_import reassembly shred
check_forbidden_import reassembly app

# ARD-0001/ADR-0002: mod.rs contains declarations and re-exports only.
mod_rs_item_defs_log="/tmp/sof-arch-check-modrs-item-definitions.log"
if search_mod_rs \
  "${mod_rs_item_defs_log}" \
  '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(fn|struct|enum|trait|impl|type|const|static)\b' \
  crates/sof-observer/src; then
  echo "ARD/ADR violation: mod.rs must not define executable or domain items"
  cat "${mod_rs_item_defs_log}"
  violations=1
fi

mod_rs_inline_mod_log="/tmp/sof-arch-check-modrs-inline-mod.log"
if search_mod_rs \
  "${mod_rs_inline_mod_log}" \
  '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\{' \
  crates/sof-observer/src; then
  echo "ARD/ADR violation: mod.rs must not contain inline module bodies"
  cat "${mod_rs_inline_mod_log}"
  violations=1
fi

if [[ "${violations}" -ne 0 ]]; then
  exit 1
fi
