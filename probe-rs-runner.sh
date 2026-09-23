#!/usr/bin/env bash
# Cargo runner wrapper: flashes+runs with the chip matching the active
# `board-*` feature, as recorded by build.rs (since `runner` in
# .cargo/config.toml can't itself depend on Cargo features).
#
# Cargo invokes this as `probe-rs-runner.sh <binary> [args...]` where the
# binary path looks like `target/<triple>/release/uwb_mesh`. build.rs records
# the chip per target triple (`target/<triple>/probe-rs-chip.txt`) because a
# single shared file goes stale when switching boards (build-script outputs
# are cached per target+features unit).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target_dir="${CARGO_TARGET_DIR:-$script_dir/target}"
binary="${1:-}"

chip_file=""
# Derive "<triple>" from ".../target/<triple>/release/<binary>".
if [[ "$binary" =~ (^|/)target/([^/]+)/[^/]+/[^/]+$ ]]; then
    triple="${BASH_REMATCH[2]}"
    chip_file="$target_dir/$triple/probe-rs-chip.txt"
fi

# Legacy fallback: single shared file (pre-per-triple builds).
if [ -z "$chip_file" ] || [ ! -f "$chip_file" ]; then
    chip_file="$target_dir/probe-rs-chip.txt"
fi

if [ ! -f "$chip_file" ]; then
    echo "error: $chip_file not found (build.rs should have written it) — falling back to STM32L431CC" >&2
    chip="STM32L431CC"
else
    chip="$(cat "$chip_file")"
fi

exec probe-rs run --chip "$chip" "$@"
