// `cargo build` of a `cdylib` that uses PyO3 needs platform-specific linker
// flags to leave Python C-API symbols undefined at link time (Python resolves
// them at runtime when it loads the extension module). On macOS in particular
// the linker won't accept undefined symbols by default and rejects the build
// without `-undefined dynamic_lookup`.
//
// `maturin build` / `maturin develop` handle this transparently, but our
// Rust CI runs raw `cargo build --workspace --all-features`, which would
// otherwise fail for `radish-python` on macOS. Calling
// `pyo3_build_config::add_extension_module_link_args()` here emits the
// right `cargo:rustc-link-arg` directives per platform.

fn main() {
    pyo3_build_config::add_extension_module_link_args();
}
