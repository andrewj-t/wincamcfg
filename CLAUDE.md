# CLAUDE.md

Guidance for Claude Code when working in this repository. Anything the code or its doc comments already say is left out; read `src/main.rs` and `src/webcam.rs` (module docs first) for the architecture.

## What this is

**wincamcfg** is a Windows-only CLI that reads and writes webcam properties through DirectShow. Two source files: `src/main.rs` (clap CLI, output, exit codes) and `src/webcam.rs` (COM, DirectShow, value parsing). Keep it that shape: no lib/bin split, no mocking trait, no extra crates unless they remove code.

Branches: `main` releases; feature work happens on short-lived branches merged via PR. Push the branch and stop; the maintainer opens PRs.

## Commands

```bash
cargo build --release
cargo test                                      # unit tests, no camera needed
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings       # CI runs this with --locked; every warning fails
cargo +1.88.0 check --locked --all-targets      # MSRV check (rustup toolchain install 1.88.0 first)
```

`rust-toolchain.toml` pins the compiler for CI, releases and local builds; bump it in its own PR so new clippy findings are reviewed on their own. `rust-version` in Cargo.toml is the MSRV and is checked separately in CI.

Debug output: `$env:RUST_LOG="trace"; cargo run -- list` (levels only, no per-target filters; goes to stderr).

## Things the code cannot tell you

- **Verify against a real camera.** The unit tests cover parsing, validation and output; everything that touches COM is checked by hand with `cargo run -- list`, `get` and `set`. The `dialog` subcommand shows the driver's own property pages, which is the reference for what "correct" looks like. This machine has a Logitech C920 and an OBS virtual camera.
- **Do not remove the re-bind in write verification.** After writing, `Device::read_back` opens a fresh handle on purpose. Some cameras (C920, PowerlineFrequency) report a written value through the same handle and drop it when the last handle closes; only a fresh handle reveals that. The UVC class driver stores the value in the device's registry parameters and applies it at the next device start, which is what `Device::stored_value` and `--restart-device` are for. Details and the experiments behind them are in TROUBLESHOOTING.md and the 0.4.0 changelog entry.
- **Lint overrides use `#[expect(lint, reason = "...")]`**, never `#[allow]`. The lint set in Cargo.toml is Microsoft's Pragmatic Rust Guidelines set; `clippy.toml` whitelists a few proper nouns for `doc_markdown`.
- **Every `unsafe` block wraps one FFI call and has a `// SAFETY:` comment**; clippy enforces the comment. No function is `unsafe fn`.
- **Do not set `RUSTFLAGS` in CI.** It replaces the hardening flags in `.cargo/config.toml` (see the comments there for what they are and how to verify them with `dumpbin`).
- **The manifest and version resource** are embedded by `build.rs`; the architecture comes from the build target.

## CI/CD

All actions are pinned to commit SHAs; Dependabot refreshes the pins. Workflows: `ci.yml` (fmt, clippy, build, test, MSRV, outdated-lockfile warning, CodeQL after build), `audit.yml` (`cargo audit`, weekly too), `dependency-review.yml`, `release.yml` (release-plz, below), `claude.yml` (`@claude` mentions from owners, members and collaborators only).

`ci.yml` has no path filter on purpose: "Build and Test" is the required check on `main` (ruleset "Protect main"), so it must report on docs-only PRs. Admins can bypass via the UI.

## Dependencies

Dependabot raises cargo PRs for security advisories only and refreshes GitHub Actions pins. Neither releases anything by itself. Routine freshness is manual: when changing the repo for another reason, run `cargo update` and include `Cargo.lock` in the same PR.

## Releases

release-plz (`release-plz.toml`, `.github/workflows/release.yml`) owns the version and CHANGELOG.md:

1. Commit subjects are Conventional Commits and become the changelog. `feat` lands under Added and bumps the minor version; `fix` under Fixed; `perf`, `refactor` and `build` under Changed; `docs` under Documentation; `build(deps)` under Dependencies; `ci`, `test` and `style` are omitted. A `!` or `BREAKING CHANGE` bumps the major. With squash merges the PR title is the subject, so get the title right.
2. After each merge to `main`, release-plz opens or updates one release PR (label `release`) with the version bump and the new changelog section, inserted directly under the file's preamble. It is opened with the `RELEASE_PLZ_TOKEN` fine-grained PAT (Contents and Pull requests read/write, this repo only) so CI runs on it; the token expires and must be renewed in the browser, there is no API for it. Never edit the version or CHANGELOG.md by hand in a feature PR.
3. Merging the release PR tags `v<version>`, creates the GitHub release with that changelog section as its body, and uploads the attested `wincamcfg.exe` and SBOMs. The crate is not on crates.io (`publish = false`, `git_only = true`); the last release is read from the `v*` tags.
