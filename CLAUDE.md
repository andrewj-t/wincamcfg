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

Debug logging via `RUST_LOG` environment variable:
```powershell
$env:RUST_LOG="trace"; cargo run -- list
$env:RUST_LOG="wincamcfg=trace"; cargo run -- set --camera 0 --property PowerlineFrequency --value 50Hz
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

- **ci.yml** — Runs on PRs/pushes to main/develop: fmt check, clippy, build, test, uploads artifact.
- **release.yml** — Triggered on push to main: version check, changelog update, git tag, SBOM generation (SPDX + CycloneDX), build attestations, GitHub release.
- **auto-patch-bump.yml** — Auto-bumps patch version on Dependabot PRs and adds changelog entry.

## Release Process

Version is in `Cargo.toml`. Pushing to `main` with a new version triggers the full release pipeline (tag, SBOM, attestation, GitHub release). The version must differ from the last release tag.
