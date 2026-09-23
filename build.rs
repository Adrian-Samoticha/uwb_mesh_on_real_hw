//! Picks the linker memory layout for the selected `board-*` feature and
//! exposes it to the linker as `memory.x`, since the two boards have
//! different flash/RAM sizes. Also records the matching probe-rs chip name
//! so `probe-rs-runner.sh` (used as the cargo `runner`) flashes the right
//! chip instead of a hardcoded one.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set"));
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));

    let (memory_x, chip): (&str, &str) = if env::var_os("CARGO_FEATURE_BOARD_BU03").is_some() {
        (include_str!("memory-bu03.x"), "STM32F103CB")
    } else {
        (include_str!("memory-l431.x"), "STM32L431CC")
    };
    fs::write(out_dir.join("memory.x"), memory_x).expect("failed to write memory.x");

    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("target"));
    // Record the chip per target triple, not in one shared file: cargo caches
    // build-script outputs per (target, features) unit, so a single shared
    // file goes stale when switching boards (e.g. L431 binary flashed with
    // the F103 chip left over from a BU03 build). `probe-rs-runner.sh`
    // derives the triple from the binary path it is given.
    let triple = env::var("TARGET").expect("TARGET not set");
    let triple_dir = target_dir.join(&triple);
    fs::create_dir_all(&triple_dir).expect("failed to create target triple dir");
    fs::write(triple_dir.join("probe-rs-chip.txt"), chip).expect("failed to write probe-rs-chip.txt");

    println!("cargo:rustc-link-search={}", out_dir.display());
    println!("cargo:rerun-if-changed=memory-l431.x");
    println!("cargo:rerun-if-changed=memory-bu03.x");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_BOARD_BU03");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_BOARD_L431");
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");
}
