# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
