# Development

What you need to know to change the code. [architecture.md](architecture.md) has diagrams of the modules, the device model and what `set` does step by step; the module docs at the top of each file under `src/` are the closest thing to a specification.

## Code layout

`wincamcfg` is a Windows-only CLI that reads and writes webcam properties through DirectShow. Five source files:

- `src/main.rs`: clap definitions, entry point, exit codes.
- `src/commands.rs`: one handler per subcommand.
- `src/output.rs`: result rows and their text and JSON rendering.
- `src/webcam.rs`: COM session, device enumeration, property reads and writes with read-back verification, device restart, the driver's dialog. The only module that calls Windows.
- `src/webcam/property.rs`: property identifiers, modes, labels and value parsing. No Windows calls, so it carries most of the unit tests.

Keep it that shape: no lib/bin split, no mocking trait, no extra crates unless they remove code.

Unit tests sit next to the code they cover and need no camera. Nothing that touches COM is unit-tested.

## Commands

```bash
cargo build --release
cargo test                                      # unit tests, no camera needed
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings       # CI runs this with --locked; every warning fails
cargo +1.88.0 check --locked --all-targets      # MSRV check (rustup toolchain install 1.88.0 first)
```

`rust-toolchain.toml` pins the compiler for CI, releases and local builds. Bump it in its own pull request so new clippy findings are reviewed on their own. `rust-version` in `Cargo.toml` is the MSRV and is checked separately in CI.

Debug output goes to stderr and takes a level, with no per-target filters:

```powershell
$env:RUST_LOG="trace"; cargo run -- list
```

## Verify against a real camera

The unit tests cover parsing, validation and output. Everything that touches COM is checked by hand with `cargo run -- list`, `get` and `set` after any change to `src/webcam.rs`. The `dialog` subcommand shows the driver's own property pages, which is the reference for what "correct" looks like. The maintainer's machine has a Logitech C920 and an OBS virtual camera, which between them cover a device with a full property set and a device with none.

## Things the code cannot tell you

- **Do not remove the re-bind in write verification.** `Device::write_all` reads every accepted write back through a fresh handle (`read_back`) on purpose. Some cameras (the C920, for `PowerlineFrequency`) report a written value through the same handle and drop it when the last handle closes; only a fresh handle reveals that. The UVC class driver stores the value in the device's registry parameters and applies it at the next device start, which is what `stored_value`, `Persistence::Stored` and `--restart-device` are for. Details and the experiments behind them are in [troubleshooting.md](troubleshooting.md) and the 0.4.0 entry in `CHANGELOG.md`.
- **Lint overrides use `#[expect(lint, reason = "...")]`**, never `#[allow]`. The lint set in `Cargo.toml` is Microsoft's Pragmatic Rust Guidelines set; `clippy.toml` whitelists a few proper nouns for `doc_markdown`.
- **Every `unsafe` block wraps one FFI call and has a `// SAFETY:` comment**; clippy enforces the comment. No function is `unsafe fn`.
- **Do not set `RUSTFLAGS` in CI.** It replaces the hardening flags in `.cargo/config.toml`; the comments there say what they are and how to verify them with `dumpbin`.
- **The manifest and version resource** are embedded by `build.rs`; the architecture comes from the build target.

## Branches and pull requests

`main` releases. Feature work happens on short-lived branches merged via pull request. Commit subjects are [Conventional Commits](https://www.conventionalcommits.org/); with squash merges the pull request title becomes the subject, so get the title right.

## CI/CD

All actions are pinned to commit SHAs; Dependabot refreshes the pins. The workflows are `ci.yml` (fmt, clippy, build, test, MSRV, outdated-lockfile warning, CodeQL after build), `audit.yml` (`cargo audit`, weekly too), `dependency-review.yml`, `release.yml` (release-plz, below) and `claude.yml` (`@claude` mentions from owners, members and collaborators only).

`ci.yml` has no path filter on purpose: "Build and Test" is the required check on `main` (ruleset "Protect main"), so it must report on docs-only pull requests. Admins can bypass via the UI.

## Dependencies

Dependabot raises cargo pull requests for security advisories only, and refreshes GitHub Actions pins. Neither releases anything by itself. Routine freshness is manual: when changing the repo for another reason, run `cargo update` and include `Cargo.lock` in the same pull request.

## Releases

release-plz (`release-plz.toml`, `.github/workflows/release.yml`) owns the version and `CHANGELOG.md`:

1. Commit subjects become the changelog. `feat` lands under Added and bumps the minor version; `fix` under Fixed; `perf`, `refactor` and `build` under Changed; `docs` under Documentation; `build(deps)` under Dependencies; `ci`, `test` and `style` are omitted. A `!` or `BREAKING CHANGE` bumps the major.
2. After a merge to `main` that contains a `feat`, `fix`, `perf`, `refactor` or `build` commit (`release_commits`), release-plz opens or updates one release pull request (label `release`) with the version bump and the new changelog section, inserted directly under the file's preamble. It is opened with the `RELEASE_PLZ_TOKEN` fine-grained PAT (Contents and Pull requests read/write, this repo only) so CI runs on it; the token expires and must be renewed in the browser, there is no API for it. Never edit the version or `CHANGELOG.md` by hand in a feature pull request.
3. Merging the release pull request tags `v<version>`, creates the GitHub release with that changelog section as its body, and uploads the attested `wincamcfg.exe` and SBOMs. The crate is not on crates.io (`publish = false`, `git_only = true`); the last release is read from the `v*` tags.
