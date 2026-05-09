## [0.2.15] - 2026-05-09

### Changed
- Update Rust dependencies: clap 4.6.0→4.6.1, indexmap 2.13→2.14, serde_with 3.18→3.19, plus transitive bumps (hashbrown, cc, libc, wasm-bindgen, etc.)

## [0.2.14] - 2026-05-01

### Changed
- Update GitHub Actions workflow dependencies: actions/checkout v4→v6, softprops/action-gh-release v2→v3

Note: no application code or Rust dependency changes in this release — the version was bumped by the auto-patch-bump workflow on a workflow-only Dependabot PR. The trigger condition has since been corrected (see #40) so future workflow-only updates will not bump the app version.

## [0.2.13] - 2026-04-01

### Changed
- Automated dependency updates

## [0.2.11] - 2026-03-01

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

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.2]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.2
[0.2.0]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.2.0
[0.1.0]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.1.0




