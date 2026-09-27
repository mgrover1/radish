# Releasing `radish-rs` to PyPI

The Python package is published to PyPI as **`radish-rs`** (the `radish`
slot is owned by an unrelated upstream). The `import radish` statement
stays unchanged regardless — that's a different namespace.

## One-time setup

1. **Create a PyPI account** at <https://pypi.org/account/register/> →
   verify email → enable 2FA.

2. **Configure trusted publishing** (no token to rotate):
   1. PyPI → *Account* → *Publishing* → *Add a pending publisher*.
   2. Owner = `aladinor`, repository = `radish`, workflow filename =
      `release.yml`, environment = `pypi`.
   3. Save. After the first successful release, this becomes a
      "trusted publisher" — no API token required ever after.

3. **Configure the GitHub environment** at
   `https://github.com/aladinor/radish/settings/environments` →
   *New environment* → name `pypi`. (Required for the OIDC handshake.)

   *Alternative*: if you'd rather use an API token, generate one at
   <https://pypi.org/manage/account/token/>, scope it to `radish-rs`,
   add as repo secret `PYPI_TOKEN`, and replace the `pypa/gh-action-pypi-publish`
   step in `.github/workflows/release.yml` with
   `with: { password: ${{ secrets.PYPI_TOKEN }} }`.

## Cutting a release

```bash
# 1. Bump version in lockstep across Cargo.toml + python/pyproject.toml.
#    The bump script validates semver, sed-replaces both files, and
#    refreshes Cargo.lock so a single commit captures the change.
./scripts/bump-version.sh 0.1.1

# 2. Run the full test matrix locally. Fixture-gated tests need
#    RADISH_NEXRAD_FIXTURE_DIR (or the legacy single-file
#    RADISH_NEXRAD_FIXTURE) set; see radish/tests/fixtures/CORPUS.md
#    for the canonical layout and download instructions.
export RADISH_NEXRAD_FIXTURE_DIR="$HOME/.cache/radish/fixtures/nexrad"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --release -p radish
cargo test --release -p radish -- --ignored   # parity tests vs danielway/nexrad
cd python && ../.venv/bin/python -m pytest tests/ && cd ..

# 3. Commit + tag (push the tag last — it's what fires the workflow)
git add Cargo.toml Cargo.lock python/pyproject.toml
git commit -m "release: v0.1.1"
git tag v0.1.1
git push origin main
git push origin v0.1.1     # ← triggers PyPI publish
```

The tag push triggers
[`.github/workflows/release.yml`](../.github/workflows/release.yml), which:

1. Creates a GitHub Release with auto-generated notes.
2. Builds wheels for **3 targets × 2 Python versions = 6 wheels**:
   - Linux x86_64 (manylinux_2_28)
   - macOS x86_64 + arm64
   - Each on Python 3.12, 3.13 (3.9-3.11 dropped: 3.9 EOL Oct 2025;
     internal NEXRAD decoder uses PEP 604 `Path | None` requiring 3.10+)

   Linux aarch64 and Windows are deferred (cross-compile linker
   can't find aarch64 libhdf5/libnetcdf, and `hdf5-metno-sys` chokes
   on the vcpkg static-md triplet on Windows).
3. Builds the sdist as a fallback (`pip install` users without a
   matching wheel build from source — needs Rust + libnetcdf-dev +
   libhdf5-dev on their machine).
4. Uploads all 7 artefacts to PyPI in one batch via the official
   `pypa/gh-action-pypi-publish` action with OIDC trusted publishing.

End-to-end tag-to-PyPI takes ~15 minutes.

## Verifying

```bash
# Wait ~15 min after the tag push, then:
uv venv /tmp/radish-test && cd /tmp/radish-test
uv pip install radish-rs
.venv/bin/python -c "import radish; print(radish.__version__)"
```

Or check the project page directly: <https://pypi.org/project/radish-rs/>.

## crates.io

The Rust crate is **not** published to crates.io — the `radish` crate
slot there is taken (different project) and we'd need to rename the
crate to `radish-rs` (which would change every `radish::` user-facing
import). Deferred until there's a clear need; until then the crate
ships only via the PyPI wheels (which embed it as a static `.so`).
