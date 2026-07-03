# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**wincamcfg** is a Windows-only CLI utility for managing webcam properties via DirectShow/COM APIs. Primary use case: fixing powerline frequency flickering (50Hz/60Hz) and configuring camera properties programmatically.

- Rust 2024 edition, Windows-only target
- Branches: `main` (releases), `develop` (active development)

## Build & Development Commands

```bash
cargo build --release          # Build optimized binary
cargo test --verbose           # Run tests
cargo fmt --all -- --check     # Check formatting (CI enforces this)
cargo fmt --all                # Auto-format code
cargo clippy -- -D warnings    # Lint with all warnings as errors (CI enforces this)
```

Debug logging via `RUST_LOG` environment variable. Accepted values:
`trace`, `debug`, `info`, `warn`, `error`, `off`. Defaults to `warn`.

```powershell
$env:RUST_LOG="trace"; cargo run -- list
$env:RUST_LOG="debug"; cargo run -- set --camera 0 --property PowerlineFrequency --value 50Hz
```

## Architecture

Two source files with clear separation of concerns:

- **`src/main.rs`** — CLI layer: argument parsing (clap derive), input validation, output formatting (text/JSON). Commands: `list`, `get`, `set`, `version`.
- **`src/webcam.rs`** — DirectShow abstraction: COM initialization (RAII `ComGuard`), device enumeration, property querying/setting via `IAMVideoProcAmp` and `IAMCameraControl` interfaces.

### Key Design Patterns

- **ComGuard** — RAII wrapper ensuring `CoInitializeEx`/`CoUninitialize` pairing. All COM operations must occur within its scope.
- **Dual property enums** — `VideoProcAmpProperty` (14 variants: Brightness, PowerlineFrequency, etc.) and `CameraControlProperty` (7 variants: Exposure, Focus, etc.) are separate types dispatched to different DirectShow interfaces.
- **Generic property handler** — `get_properties()` uses compile-time polymorphism to handle both property types with a single template.
- **Human-readable value formatting** — Round-trip parsing between user strings ("50Hz", "Auto", "On") and DirectShow i32 values via `format_property_value()`/`parse_property_value()`.
- **IndexMap** for ordered output — Preserves property order in JSON serialization.

### build.rs

Embeds a Windows application manifest (amd64, asInvoker, Windows 10/11 compatibility, SegmentHeap) and version info from Cargo.toml using `winresource`.

## CI/CD

- **ci.yml** — Runs on PRs/pushes to main/develop: fmt check, clippy, build, test, artifact upload; a CodeQL job (rust + actions) runs only after build/test pass.
- **release.yml** — Triggered via `workflow_run` after CI succeeds on main: skips quietly if the version's tag already exists, otherwise builds the release binary, tags the CI-validated commit, generates SBOMs (SPDX + CycloneDX), attests, and creates the GitHub release. It does NOT push to main (branch protection rejects workflow pushes).
- **auto-patch-bump.yml** — Auto-bumps patch version on Dependabot cargo PRs and adds changelog entry.

Branch protection on `main` (ruleset "Protect main") requires the "Build and Test" check; admins can bypass deliberately via the UI.

## Dependency Policy

Dependabot is limited to security updates only (`open-pull-requests-limit: 0` in `.github/dependabot.yml`). Routine freshness is manual: when making changes to the repo for any other reason, also run `cargo update` and include the refreshed `Cargo.lock` in the same PR.

## Release Process

Version is in `Cargo.toml`. Merging to `main` with a new version triggers the release pipeline automatically once CI passes. The version must not already have a release tag, and CHANGELOG.md must be updated in the same PR that bumps the version (the pipeline no longer writes the changelog).
