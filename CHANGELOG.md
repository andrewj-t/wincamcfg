# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-09-22

### Added
- `wincamcfg --version` flag (the `version` subcommand remains).
- `wincamcfg dialog --camera N` opens the driver's own property dialog (the pages OBS Studio shows under *Configure Video*) for visual confirmation of what `get` reports.
- `get` shows `Range: min..max` (and the step when it is not 1) for numeric properties and adds `min`, `max` and `step` to the JSON output.
- Exit codes: 0 success, 1 usage or enumeration error, 2 when `set` completed but at least one property write failed. Previously `set` always exited 0.
- Hardware-free unit tests for value parsing and formatting, mode reporting, range and capability validation, camera selection and CLI parsing. CI's `cargo test` step was previously a no-op.
- `SECURITY.md` with a private disclosure channel.
- Control Flow Guard is enabled for MSVC builds via `.cargo/config.toml`.
- `rust-toolchain.toml` pins the compiler used by CI, releases and local builds; `rust-version` (1.88) declares the MSRV and CI checks it.
- Weekly `cargo audit` workflow so advisories against unchanged dependencies are caught.

### Changed
- `--value Auto` on `PowerlineFrequency` now selects the driver's Auto value (3) instead of writing 0 with the Auto flag.
- Property names are matched case-insensitively throughout (`--property brightness` used to fail after being found).
- `--default` writes the device's numeric default directly instead of round-tripping through a display string.
- Requesting Auto on a property that does not advertise Auto capability is rejected up front; manual values are range-checked before the driver is called. Auto writes keep the current value instead of sending 0.
- `--camera all` skips devices that lack the requested property (with a notice) instead of aborting on the first virtual camera; a specific camera index still reports an error.
- `list` only reads device names and paths; it no longer binds every device's filter and queries every property.
- Properties are listed in the order of the standard DirectShow property dialog's tabs (Brightness, Contrast, Hue, Saturation, Sharpness, Gamma, WhiteBalance, BacklightCompensation, Gain, ColorEnable, PowerlineFrequency; Zoom, Focus, Exposure, Iris, Pan, Tilt, Roll).
- Logs go to stderr, so `--output json` stays machine-readable with `RUST_LOG` set. An unparseable `RUST_LOG` value prints a warning instead of silently falling back to `warn`.
- A property whose current value cannot be read is shown as `<unavailable>` instead of `0`.
- `set` reports the canonical label (`50Hz`, `Auto`) in its `value` field rather than the raw text typed by the user.
- One COM session and one enumeration per command instead of re-initialising COM and re-enumerating for every property write. Every `unsafe` block is now narrowed to the FFI call and documented.
- Adopted the Microsoft Pragmatic Rust Guidelines lint set (`[lints]` in `Cargo.toml`); CI runs clippy with `--all-targets` and `--locked`.
- `windows` crate pinned to 0.62 (was a `>=0.59, <=0.62` range); `tracing-subscriber` trimmed to `fmt` + `std`; release profile uses fat LTO, `panic = "abort"`, stripped symbols and overflow checks.
- `build.rs` derives the manifest architecture from the build target (arm64 builds no longer claim amd64).
- CI: every GitHub Action pinned to a commit SHA, least-privilege permissions per job, credential-free checkouts, no PR-writable build cache in release builds, `cargo-sbom` pinned. Dependabot now refreshes action pins.
- Releases are driven by release-plz: every merge to `main` updates a single release PR (version bump plus generated changelog section from Conventional Commit messages), and merging that PR tags, creates the GitHub release and uploads the attested binary and SBOMs. This replaces the `workflow_run` release chain and the Dependabot auto-patch-bump job, so dependency updates no longer release on their own.
- `CHANGELOG.md` header restored to the top of the file and a duplicated 0.2.11 entry removed.

### Removed
- Stale branches `chore/trim-deps` (already merged as 0.3.0), `refactor/ms-rust-guidelines` and `feature/trait-based-mocking` (their useful parts are re-implemented here; the mocking trait had no production implementor).

### Notes
- `0.3.1` appears in this changelog but was never tagged or released.

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
