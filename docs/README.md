# Radish documentation

Long-form documentation for the [`radish`](https://github.com/aladinor/radish)
weather-radar library. The repo-root `README.md` is the entry point;
this folder is for content that's too detailed to fit there.

## Contents

| File | What it covers |
| --- | --- |
| [`GETTING_STARTED.md`](GETTING_STARTED.md) | End-to-end install + first-read walkthrough (Rust + Python) |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Data model, backend trait, design rationale, Mermaid diagrams |
| [`PROJECT_SUMMARY.md`](PROJECT_SUMMARY.md) | Phased roadmap and current implementation status |
| [`CHANGELOG.md`](CHANGELOG.md) | Version history (Keep a Changelog format) |
| [`NEXRAD_LEVEL3_WASM.md`](NEXRAD_LEVEL3_WASM.md) | Design: a NEXRAD Level 3 (NIDS) backend, and a `wasm32` target so radish decodes in the browser. Written for a session with no prior context; cross-repo paths are absolute |
| `RELEASING.md` | Release walkthrough — added by the `ci/release-pipeline` PR (link wired up once that lands) |

## Conventions

- **Filenames** use `SCREAMING_SNAKE.md` for top-level guides (matches the
  pre-`docs/` layout) and lowercase for subordinate notes.
- **CHANGELOG.md** follows
  [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/) and
  [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html). Every
  PR that ships user-visible behaviour should add an entry under
  `[Unreleased]`; the release script bumps that to a dated section.
- **Cross-references** between docs use repo-relative paths
  (e.g. `docs/ARCHITECTURE.md`) so they resolve correctly from the
  repo root, where most readers land.

## Where other docs live

A handful of operational files intentionally stay at the repo root —
they're not narrative documentation:

- [`README.md`](../README.md) — project entry point, displayed by GitHub
  and PyPI.
- [`CLAUDE.md`](../CLAUDE.md) — instructions for Claude Code agents
  working on this repo.
