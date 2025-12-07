# 📷 wincamcfg

> A command-line utility for managing webcam configuration on Windows

## The Problem 🤔

Ever moved to a country with 50Hz powerline frequency and noticed your webcam footage looking like a disco strobe light? Windows defaults to 60Hz anti-flicker settings, which causes annoying flickering when your local power grid runs at 50Hz. While you *can* fix this manually in camera settings... doing it for multiple cameras or at scale is a pain.

That's where `wincamcfg` comes in! 🎉

## What It Does ✨

`wincamcfg` is a simple command-line tool that lets you configure webcam properties programmatically. Whether you need to fix powerline frequency issues, adjust brightness and contrast, or reset all cameras to default settings, this tool has you covered.

It should be able to set the same settings as the DirectShow Native settings dilog which you may be familiar with:

![NativeCameraControls](NativeCameraControls.png)

## Installation 🚀

### From Source

```bash
git clone https://github.com/andrewj-t/wincamcfg.git
cd wincamcfg
cargo build --release
```

The compiled binary will be in `target/release/wincamcfg.exe`.

## Usage 💡

### List All Cameras

See what cameras are connected to your system:

```bash
wincamcfg list
```

Example output:

```text
[0] Integrated Webcam
[1] Logitech HD Pro C920
```

### Get Current Settings

Check current property values for a specific camera:

```bash
# Get all properties for camera 0
wincamcfg get --camera 0

# Get all properties for all cameras
wincamcfg get --camera all

# Output as JSON for scripting
wincamcfg get --camera 0 --output json
```

### Fix Powerline Frequency Flickering

The main reason this tool exists! Set your cameras to match your local power grid:

```bash
# Set camera 0 to 50Hz (for most of Europe, Asia, Africa, Australia)
wincamcfg set --camera 0 --property PowerlineFrequency --value 50Hz

# Set camera 0 to 60Hz (for Americas, parts of Asia)
wincamcfg set --camera 0 --property PowerlineFrequency --value 60Hz

# Set ALL cameras to 50Hz
wincamcfg set --camera all --property PowerlineFrequency --value 50Hz
```

### Adjust Other Properties

You can configure many other camera settings:

```bash
# Adjust brightness
wincamcfg set --camera 0 --property Brightness --value 128

# Adjust contrast
wincamcfg set --camera 0 --property Contrast --value 150

# Enable auto white balance
wincamcfg set --camera 0 --property WhiteBalance --value Auto

# Disable backlight compensation
wincamcfg set --camera 0 --property BacklightCompensation --value Off
```

### Reset to Defaults

Restore factory settings:

```bash
# Reset a specific property to default
wincamcfg set --camera 0 --property Brightness --default

# Reset ALL properties on a camera to defaults
wincamcfg set --camera 0 --property all --default

# Reset ALL cameras to factory defaults
wincamcfg set --camera all --property all --default
```

## Available Properties 🎛️

- `PowerlineFrequency` - Fix flickering (Disabled, 50Hz, 60Hz, Auto)
- `Brightness` - Adjust brightness levels
- `Contrast` - Adjust contrast levels
- `Hue` - Adjust colour hue
- `Saturation` - Adjust colour saturation
- `Sharpness` - Adjust image sharpness
- `Gamma` - Adjust gamma correction
- `WhiteBalance` - White balance (Auto or manual value)
- `BacklightCompensation` - Backlight compensation (On/Off)
- `Gain` - Gain/ISO control
- `colourEnable` - Enable/disable colour (On/Off)

Use `wincamcfg get --camera 0` to see which properties your specific camera supports.

## Automation & Scripting 🤖

Perfect for deployment scenarios! Use JSON output for scripting:

```powershell
# PowerShell example: Configure all cameras on startup
wincamcfg set --camera all --property PowerlineFrequency --value 50Hz --output json
```

You can add this to startup scripts, group policies, or deployment tools to ensure consistent camera configuration across multiple machines.

## Requirements 📋

- Windows (uses DirectShow APIs)
- Rust 2024 edition or later (for building from source)

## Troubleshooting 🔧

Having issues? Check out the [Troubleshooting Guide](TROUBLESHOOTING.md) for debug logging instructions and common solutions.

## Show Your Support ⭐

If this tool helped fix your flickering webcam or made your life easier, a star on GitHub would be much appreciated!

## License 📄

MIT License - See [LICENSE](LICENSE) for details.

## Contributing 🤝

Found a bug or want to add a feature? PRs are welcome! Please include working code with your contribution.

---

Made with ❤️ because flickering webcams are annoying.
