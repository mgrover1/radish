#!/usr/bin/env bash
# Bump radish-rs version in lockstep across the Rust workspace and the
# Python pyproject. Run from the repo root:
#
#     ./scripts/bump-version.sh 0.1.1
#
# Then commit + tag (the script prints the commands) so the
# `release.yml` workflow fires and publishes to PyPI.
#
# Why this script exists: the Rust workspace `Cargo.toml`, the Python
# `pyproject.toml`, and (for tagged releases) the git tag itself all
# need to agree on the version string. Bumping them by hand is
# error-prone — one out-of-sync file ships a wheel whose internal
# `__version__` doesn't match its filename, and pip caches that
# permanently. One script, one source of truth.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <new-version>" >&2
  echo "example: $0 0.1.1" >&2
  exit 64
fi

NEW=$1
REPO=$(git rev-parse --show-toplevel)
cd "$REPO"

# Validate semver-ish (X.Y.Z, optional pre-release/build).
if ! [[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.+-]+)?$ ]]; then
  echo "error: version must look like X.Y.Z (got: $NEW)" >&2
  exit 64
fi

OLD=$(grep -oP '^version = "\K[^"]+' Cargo.toml | head -n1)
echo "Bumping ${OLD} → ${NEW}"

# 1. Workspace Cargo.toml — drives every member crate via `version.workspace = true`.
sed -i "s/^version = \"${OLD}\"$/version = \"${NEW}\"/" Cargo.toml

# 2. Python pyproject.toml — independent file, must match.
sed -i "s/^version = \"${OLD}\"$/version = \"${NEW}\"/" python/pyproject.toml

# Sanity-check both files agree on the new value.
RUST_V=$(grep -oP '^version = "\K[^"]+' Cargo.toml | head -n1)
PY_V=$(grep -oP '^version = "\K[^"]+' python/pyproject.toml | head -n1)
if [[ "$RUST_V" != "$NEW" || "$PY_V" != "$NEW" ]]; then
  echo "error: post-bump version mismatch — Rust=$RUST_V, Python=$PY_V" >&2
  exit 1
fi

# Refresh Cargo.lock so it doesn't show the bump as a separate diff.
cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null

echo "Done. Next:"
echo "  git add Cargo.toml Cargo.lock python/pyproject.toml"
echo "  git commit -m 'release: v${NEW}'"
echo "  git tag v${NEW}"
echo "  git push origin main"
echo "  git push origin v${NEW}    # ← triggers PyPI publish"
