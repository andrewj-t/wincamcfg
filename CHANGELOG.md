# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0](https://github.com/andrewj-t/wincamcfg/compare/v0.4.0...v0.5.0) - 2026-09-25

### Added

- Show the device path in get
- Show the driver's details in get

### Changed

- Clarify set, write verification and output after the module split
- Split into commands, output and webcam/property modules
- Trim comments and compact the remaining verbose code
- Use windows-registry, IsUserAnAdmin and owned VARIANT; move write verification into webcam.rs
- One Property enum, parser-based validation, fewer duplicate impls

### Fixed

- Log every read-back at trace level

### Documentation

- Split documentation by audience under docs/
- Add architecture diagrams under docs/
- Correct the SBOM attestation verification commands

## [0.4.0] - 2026-09-22

A cleanup of the whole project. It fixes several real bugs in `set`, makes the tool honest about what happened to a write, tightens the build and the CI pipeline, and hands releases to release-plz. It also settles something that came up during testing: on some cameras a powerline frequency write seems to vanish. It does not. See "Writes that take effect later" below.

### Added
- `set` reads every write back through a fresh handle. If the camera drops a value once nothing has it open (a Logitech C920 does this for PowerlineFrequency), the result says so instead of claiming success. The Windows UVC driver still stores the value and applies it when the camera next starts, and the message tells you that: `stored by the driver and applied when the camera next starts (reconnect it or reboot)`. JSON results carry the same text in a `note` field.
- `set --restart-device` restarts the camera when a value was only stored, so it takes effect at once. It needs an elevated prompt and interrupts anything using the camera, which is why it is opt in.
- `dialog --camera N` opens the driver's own property pages, the same window OBS shows under Configure Video. Useful for checking what the tool reports against what Windows shows.
- `get` prints the range of numeric properties (`Range: 0..255`, plus the step when it is not 1), and JSON output includes `min`, `max` and `step`.
- Exit codes that mean something: 0 when everything worked, 1 for a usage or enumeration error, 2 when `set` ran but at least one write failed. Until now `set` exited 0 no matter what.
- `wincamcfg --version`.
- 28 unit tests that run without a camera. CI ran `cargo test` before this release, against an empty suite.
- `SECURITY.md`, so there is a private way to report a vulnerability.
- Hardening flags for MSVC builds in `.cargo/config.toml`: Control Flow Guard, CET shadow stack compatibility, and a dependent load flag that makes the loader take the executable's imports from System32 only.
- `rust-toolchain.toml` pins the compiler (1.97.1). `rust-version` declares the oldest supported one (1.88) and CI checks it.
- A weekly `cargo audit` run, so an advisory against a dependency that has not changed still gets noticed.

### Changed
- `--value Auto` on PowerlineFrequency picks the driver's Auto setting (value 3). It used to write 0 with the Auto flag, which is not the same thing.
- Property names match regardless of case everywhere. `--property brightness` used to find the property and then fail on it.
- `--default` sends the numeric default straight to the driver and, for properties that can run in Auto, switches Auto back on. That is what the Default button in the Windows dialog does. The old code left them in Manual.
- Values are checked against the reported range and capabilities before the driver is called, so an out of range value, or Auto on a property that only does Manual, fails with a clear message. An Auto write keeps the current value instead of sending 0.
- With `--camera all`, a device that lacks the property is skipped with a notice. Before, the first virtual camera without it aborted the whole command.
- `list` reads only names and paths. It no longer opens every device and queries every property.
- Properties are listed in the same order as the tabs of the Windows dialog.
- Logs go to stderr, so `--output json` stays parseable with `RUST_LOG` set. A misspelt `RUST_LOG` value prints a warning instead of quietly falling back to `warn`.
- A property whose current value cannot be read shows `<unavailable>` rather than `0`.
- `set` reports what it sent (`50Hz`, `4000 [Auto]`) rather than echoing what you typed.
- One COM session per command. The old code initialised COM and enumerated every device again for each property it wrote, up to 21 times for `--property all`.
- Every `unsafe` block wraps a single call into Windows and carries a comment saying why it is sound. Clippy enforces this.
- The Microsoft Pragmatic Rust Guidelines lint set is on, and CI runs clippy with `--all-targets --locked`.
- The `windows` crate is pinned to 0.62 instead of a range spanning four incompatible versions. `tracing-subscriber` is trimmed to what is used. The release profile uses fat LTO, aborts on panic, strips symbols and keeps overflow checks on.
- `build.rs` takes the manifest architecture from the build target, so an arm64 build no longer claims to be amd64 (if arm64 support were added in the future).
- CI: every GitHub Action is pinned to a commit SHA, each job has only the permissions it needs, checkouts keep credentials only when they push, `cargo` runs with `--locked`, release builds no longer restore a cache that pull requests can write to, and `cargo-sbom` is pinned. Dependabot now refreshes the action pins.
- Releases are driven by release-plz. Each merge to `main` updates a release pull request with the next version and a changelog section generated from commit messages; merging it tags, publishes the release and uploads the attested binary and SBOMs. The old `workflow_run` chain and the Dependabot auto patch bump are gone, so a dependency update no longer produces a release on its own.
- The changelog header is back at the top of this file and a duplicated 0.2.11 entry is gone.

### Removed
- The `version` subcommand. Use `--version`.
- Three stale branches: `chore/trim-deps` (already merged in 0.3.0), `refactor/ms-rust-guidelines` and `feature/trait-based-mocking`. The parts worth keeping are reimplemented here; the mocking trait had nothing real implementing it.

## [0.3.2] - 2026-07-03

### Changed
- Fix clippy `manual` `Option::zip` lint that failed CI on newer stable toolchains
- CI/CD overhaul: CodeQL now runs as a gated job after build/test, the release pipeline triggers on CI success instead of racing it, the automatic Claude Code review workflow was removed, and CHANGELOG updates move into version-bump PRs (branch protection on `main` now requires CI to pass)

## [0.3.1] - 2026-07-01

### Changed
- Automated dependency updates

## [0.3.0] - 2026-05-24

### Changed

- Surface Auto/Manual mode for camera properties in `get` output, with a `modes_supported` field listing available modes (#42)
- Fix `--value` not accepting negative numbers (#43)
- Reviewed all dependencies and look for oppertunies to reduce them and use functionality in other libraries:
  - drop `serde_with` and `strum`, drop `tracing-subscriber` env-filter feature
  - trim `clap` to a minimal feature set
  - drop redundant `windows` crate features
- Bump transitive deps to latest patches

## [0.2.15] - 2026-05-09

### Changed
- Update Rust dependencies: clap 4.6.0→4.6.1, indexmap 2.13→2.14, serde_with 3.18→3.19, plus transitive bumps (hashbrown, cc, libc, wasm-bindgen, etc.)

## [0.2.14] - 2026-05-01

### Changed
- Update GitHub Actions workflow dependencies: actions/checkout v4→v6, softprops/action-gh-release v2→v3

Note: no application code or Rust dependency changes in this release. The version was bumped by the auto-patch-bump workflow on a workflow-only Dependabot PR. The trigger condition has since been corrected (see #40) so future workflow-only updates will not bump the app version.

## [0.2.13] - 2026-04-01

### Changed
- Automated dependency updates

## [0.2.12] - 2026-03-03

### Changed
- Code quality improvements: reduce duplication and dead code (#34)

## [0.2.11] - 2026-03-01

### Changed
- Automated dependency updates

## [0.2.10] - 2026-02-11

### Changed
- Bump time from 0.3.44 to 0.3.47 
- Bumped other dependencies to latest via cargo update

## [0.2.9] - 2026-02-08

### Changed
- Automated dependency updates

## [0.2.8] - 2026-02-01

### Changed
- Automated dependency updates

## [0.2.7] - 2026-01-25

### Changed
- Automated dependency updates

## [0.2.6] - 2026-01-11

### Changed
- Feat/simplify functionality (#24)

## [0.2.5] - 2025-12-24

### Changed
- 0.2.5 - Update dependencies (#20) - 0.2.4

## [0.2.4] - 2025-12-24

### Changed
- 0.2.4 - Dependency update & CI/CD Test (#12)

## [0.2.2] - 2025-12-15

### Changed

- fix tag detection
- 0.2.2 - Refactor from chatgpt recommendations (#9)
- Update Dependabot config for GitHub Actions directory
- rs-windows crate v0.62 support (#8)
- Fix for code scanning alert no. 2: Workflow does not contain permissions (#7)

## [0.2.0] - 2025-12-10

### Changed

- Use IndexMap to preserve property order to preserve property order. (#6)
- Bump actions/upload-artifact from 4 to 5 (#2)
- Bump actions/checkout from 4 to 6 (#3)
- Bump strum from 0.26.3 to 0.27.2 (#5)
- Add Initial CI Workflow (#1)

## [0.1.0] - 2025-12-07

### Added

- Initial release
- List all connected video capture devices
- Get current property values for cameras (individual or all)
- Set camera properties with human-readable values
- Reset properties to defaults (individual or all)
- PowerlineFrequency configuration (50Hz/60Hz) to fix flickering
- Support for VideoProcAmp properties:
  - Brightness
  - Contrast
  - Hue
  - Saturation
  - Sharpness
  - Gamma
  - WhiteBalance
  - BacklightCompensation
  - Gain
  - ColorEnable
  - PowerlineFrequency
- JSON output format for scripting and automation
- Detailed device information including driver details
- Bulk operations (set all cameras, reset all properties)

[0.4.0]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.4.0
[0.3.2]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.3.2
[0.3.0]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.3.0
[0.2.15]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.15
[0.2.14]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.14
[0.2.13]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.13
[0.2.12]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.12
[0.2.11]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.11
[0.2.10]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.10
[0.2.9]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.9
[0.2.8]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.8
[0.2.7]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.7
[0.2.6]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.6
[0.2.5]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.5
[0.2.4]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.4
[0.2.2]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.2
