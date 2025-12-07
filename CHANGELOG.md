# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.0]: https://github.com/andrewj-t/wincamcfg/releases/tag/v0.1.0
