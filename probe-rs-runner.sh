#!/usr/bin/env bash
# Cargo runner wrapper: flashes+runs with the chip matching the active
# `board-*` feature, as recorded by build.rs (since `runner` in
# .cargo/config.toml can't itself depend on Cargo features).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
target_dir="${CARGO_TARGET_DIR:-$script_dir/target}"
chip_file="$target_dir/probe-rs-chip.txt"

if [ ! -f "$chip_file" ]; then
    echo "error: $chip_file not found (build.rs should have written it) — falling back to STM32L431CC" >&2
    chip="STM32L431CC"
else
    chip="$(cat "$chip_file")"
fi

exec probe-rs run --chip "$chip" "$@"
