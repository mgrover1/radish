//! Emits `--cfg has_real_fixtures` when `RADISH_NEXRAD_LEVEL3_FIXTURE_DIR`
//! is set — `wasm/tests/real_fixtures.rs` gates on that `cfg`, NOT a
//! Cargo feature. A feature was tried first and reverted: this workspace's
//! own CI (`.github/workflows/rust-ci.yml`) runs `--all-features` on
//! several steps (`cargo clippy --all-targets --all-features`, `cargo
//! build/test --all-features`, `cargo tarpaulin --all-features`), which
//! sweeps in ANY crate feature including an opt-in one — verified this
//! directly breaks the build (`cargo test -p radish-wasm --all-features
//! --no-run` fails to compile with the env var unset, since
//! `real_fixtures.rs`'s `env!()` calls are then reached unconditionally).
//! A `--cfg`, unlike a feature, is invisible to `--all-features` entirely,
//! so this is the one gating mechanism that's actually off by default
//! under every command this repo's CI runs — not just a scoped, manually-
//! invoked one.
fn main() {
    // Declare the custom cfg so rustc's `unexpected_cfgs` lint (on by
    // default since Rust 1.80) doesn't flag `#![cfg(has_real_fixtures)]`
    // as an unknown condition under `-D warnings`.
    println!("cargo::rustc-check-cfg=cfg(has_real_fixtures)");
    if std::env::var_os("RADISH_NEXRAD_LEVEL3_FIXTURE_DIR").is_some() {
        println!("cargo::rustc-cfg=has_real_fixtures");
    }
    // Re-run this script (and thus re-evaluate the cfg) whenever the env
    // var is set, unset, or changed — otherwise Cargo would cache the
    // first build's decision indefinitely.
    println!("cargo::rerun-if-env-changed=RADISH_NEXRAD_LEVEL3_FIXTURE_DIR");
}
