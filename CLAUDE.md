# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**wincamcfg** is a Windows-only CLI utility for managing webcam properties via DirectShow/COM APIs. Primary use case: fixing powerline frequency flickering (50Hz/60Hz) and configuring camera properties programmatically.

- Rust 2024 edition, Windows-only target
- Branches: `main` (releases); feature work happens on short-lived branches merged via PR

## Build & Development Commands

```bash
cargo build --release                           # Build optimized binary
cargo test                                      # Run the unit tests (no camera needed)
cargo fmt --all -- --check                      # Check formatting (CI enforces this)
cargo fmt --all                                 # Auto-format code
cargo clippy --all-targets -- -D warnings       # Lint with all warnings as errors (CI enforces this)
cargo +1.88.0 check --locked --all-targets      # MSRV check (CI enforces this; rustup toolchain install 1.88.0 first)
```

`rust-toolchain.toml` pins the exact compiler (currently 1.97.1) used by CI, releases and local builds; rustup picks it up automatically. Bump it deliberately in its own PR. `rust-version` in `Cargo.toml` (1.88) is the MSRV and is checked separately.

Debug logging via `RUST_LOG` environment variable. Accepted values: `trace`, `debug`, `info`, `warn`, `error`, `off`. Defaults to `warn`; an unrecognised value prints a warning. Logs go to stderr.

```powershell
$env:RUST_LOG="trace"; cargo run -- list
$env:RUST_LOG="debug"; cargo run -- set --camera 0 --property PowerlineFrequency --value 50Hz
```

Exit codes: 0 success, 1 usage/enumeration error, 2 when `set` ran but at least one write failed.

## Architecture

Two source files with clear separation of concerns:

- **`src/main.rs`** — CLI layer: argument parsing (clap derive), output formatting (text/JSON), exit-code mapping. Commands: `list`, `get`, `set`, `dialog`, `version`.
- **`src/webcam.rs`** — DirectShow abstraction: COM session, device enumeration, property querying/setting via `IAMVideoProcAmp` and `IAMCameraControl`, value label parsing/formatting.

### Key Design Patterns

- **`ComSession`** — proof that COM is initialised on this thread. `!Send`/`!Sync`, only constructible via `new()`, created once per command and borrowed by every COM call; `Drop` calls `CoUninitialize`. Declare it first in a command function so it drops last.
- **`Device<'com>`** — a bound capture device (holds an `IMoniker`) that borrows the session, so no COM interface can outlive `CoUninitialize`. `Device::info()` returns plain `DeviceInfo` data; `Device::set()` validates then writes.
- **`PropertyControl` trait** — implemented for `IAMVideoProcAmp` and `IAMCameraControl`; one generic `query_properties` serves both. The Auto/Manual flag bits are identical for both interfaces (compile-time asserted) and centralised in `Mode`.
- **Value parsing** — `parse_property_value` returns `ParsedValue::{Auto, Manual(i32), Default}`. Labels (`50Hz`, `On`, `Auto` for PowerlineFrequency) are checked before the `Auto` keyword. `resolve_set` (pure, unit-tested) turns a `ParsedValue` into the `Written { value, mode }` the driver gets, rejecting unsupported modes and out-of-range values; `Default` restores the default value in Auto mode where supported (what the dialog's Default button does).
- **Write verification** — after the writes for a device, `Device::read_back` re-binds the filter (a fresh handle) and `Written::persisted_in` compares. Some cameras keep a value only while a handle is open (C920 PowerlineFrequency). On a mismatch, `Device::stored_value` reads the UVC class driver's copy under `HKLM\SYSTEM\CurrentControlSet\Enum\<instance>\Device Parameters` (world-readable): if it matches, the write is a success with a note (applied at next device start); otherwise a failure. Reads through the *same* handle would not detect this, so do not "optimise" the re-bind away.
- **`Device::open_property_dialog`** — shows the driver's own property pages (`ISpecifyPropertyPages` + `OleCreatePropertyFrame`), the same window OBS opens for "Configure Video". Useful for checking the tool against what Windows shows; the dialog's Default button applies immediately and keeps Auto mode on for properties that support it.
- **Unsafe policy** — every `unsafe` block wraps exactly one FFI call and carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks` is on). No function is `unsafe fn`; none can cause UB from safe code.
- **IndexMap** for ordered output — preserves property order in JSON serialization.

### Lints and tests

`Cargo.toml` carries the Microsoft Pragmatic Rust Guidelines lint set (`[lints.rust]`, `[lints.clippy]` with all major groups plus restriction lints). CI runs clippy with `-D warnings`, so every warning is a failure. Override per item with `#[expect(lint, reason = "...")]`, never `#[allow]`. `clippy.toml` whitelists DirectShow and property names for `doc_markdown`.

Unit tests live in `#[cfg(test)] mod tests` at the bottom of each source file and need no camera: they cover parsing, formatting, mode logic, validation, camera selection and clap definitions. Anything that touches COM is verified by hand (`cargo run -- list`, `get`, `set`) against a real webcam.

### build.rs

Embeds a Windows application manifest (architecture from the build target, asInvoker, Windows 10/11 compatibility, SegmentHeap) and version info from Cargo.toml using `winresource`. `.cargo/config.toml` enables Control Flow Guard for MSVC targets; do not set a `RUSTFLAGS` environment variable in CI, it would replace those flags.

## CI/CD

All GitHub Actions are pinned to full commit SHAs with a version comment; Dependabot refreshes the pins.

- **ci.yml** — Runs on every PR and push to main: fmt check, clippy (`--all-targets --locked`), build, test, artifact upload, an MSRV check on 1.88.0, an outdated-lockfile warning on PRs, and a CodeQL job (rust + actions) that runs only after build/test pass. There is deliberately no path filter: "Build and Test" is a required check, so it must report on docs-only PRs too.
- **audit.yml** — `cargo audit` on manifest changes, pushes to main and weekly on a schedule.
- **dependency-review.yml** — GitHub dependency review on PRs, failing on moderate severity.
- **release.yml** — release-plz, on every push to main. `release-plz-pr` opens or updates the single release PR (version bump + changelog section generated from Conventional Commit messages). `release-plz-release` tags `v<version>` and creates the GitHub release when Cargo.toml's version has no tag yet (i.e. the release PR was merged). `publish-binary` then builds the tagged commit on Windows from a clean build, generates SBOMs, attests everything and uploads the assets to that release. Only the jobs that need them hold write/OIDC permissions.
- **claude.yml** — `@claude` mentions from owners, members and collaborators only.

Branch protection on `main` (ruleset "Protect main") requires the "Build and Test" check; admins can bypass deliberately via the UI.

## Dependency Policy

Dependabot raises cargo PRs for security advisories only (`open-pull-requests-limit: 0` for version updates in `.github/dependabot.yml`) and refreshes GitHub Actions pins. Neither releases anything on its own: merged updates accumulate in the release PR and ship with the next release. Routine crate freshness is manual: when making changes to the repo for any other reason, also run `cargo update` and include the refreshed `Cargo.lock` in the same PR.

## Release Process

Releases are managed by [release-plz](https://release-plz.dev) (`release-plz.toml`, `.github/workflows/release.yml`):

1. Write Conventional Commit subjects (`feat:`, `fix:`, `refactor:`, `docs:`, `build(deps):`, `ci:` ...). They become the changelog: `feat` lands under Added, `fix` under Fixed, `perf`/`refactor`/`build` under Changed, `docs` under Documentation, dependency bumps under Dependencies; `ci`, `test` and `style` are omitted. A `feat` bumps the minor version, anything else the patch, a `!`/`BREAKING CHANGE` the major.
2. After each merge to main, release-plz updates the release PR (labelled `release`). Review it like any other PR; CI runs on it because it is opened with the `RELEASE_PLZ_TOKEN` personal access token (fine-grained, this repo only, Contents and Pull requests read/write). Do not edit Cargo.toml's version or CHANGELOG.md by hand in feature PRs; the bot owns both. Keep `## [Unreleased]` directly under the CHANGELOG preamble, that is where new sections are inserted.
3. Merging the release PR tags the commit, creates the GitHub release with the changelog section as its body, and uploads the attested `wincamcfg.exe` and SBOMs.

The crate is not published to crates.io (`publish = false`, `git_only = true`); the last released version is read from the `v*` tags.
