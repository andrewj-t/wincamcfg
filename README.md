# wincamcfg

> A command-line utility for managing webcam configuration on Windows

## The problem

Ever moved to a country with 50Hz powerline frequency and noticed your webcam footage looking like a disco strobe light? Windows defaults to 60Hz anti-flicker settings, which causes annoying flickering when your local power grid runs at 50Hz. While you *can* fix this manually in camera settings... doing it for multiple cameras or at scale is a pain.

That's where `wincamcfg` comes in.

## What it does

`wincamcfg` lets you read and write webcam properties from the command line. The original use case was fixing powerline-frequency flicker on cameras moved between 50Hz and 60Hz countries, but the same approach works for brightness, contrast, white balance, and the rest of the DirectShow property set.

It can set the same things as the native DirectShow camera-properties dialog:

![NativeCameraControls](NativeCameraControls.png)

## Installation

### Download

Download the latest `wincamcfg.exe` from the [Releases page](https://github.com/andrewj-t/wincamcfg/releases) and put it somewhere on your `PATH`.

### From source

```bash
git clone https://github.com/andrewj-t/wincamcfg.git
cd wincamcfg
cargo build --release
```

The compiled binary will be in `target/release/wincamcfg.exe`.

Every release ships with build-provenance attestations and SBOMs; [docs/release-verification.md](docs/release-verification.md) explains how to check them.

## Quick start

```bash
wincamcfg list                                                       # see which cameras are connected
wincamcfg get --camera 0                                             # read every property of camera 0
wincamcfg set --camera 0 --property PowerlineFrequency --value 50Hz   # stop 50Hz flicker
wincamcfg set --camera 0 --property Exposure --value Auto             # hand exposure back to the driver
```

## Requirements

- Windows 10 or 11 (uses DirectShow APIs)
- To build from source: Rust 1.88 or later (`rust-version` in `Cargo.toml`). `rust-toolchain.toml` pins the exact compiler that CI and releases use; `rustup` picks it up automatically.

Check the installed version with `wincamcfg --version`.

## Documentation

- [docs/usage.md](docs/usage.md) walks through the common tasks.
- [docs/properties.md](docs/properties.md) lists every property and what it accepts.
- [docs/reference.md](docs/reference.md) is the full command, flag, value, exit-code and JSON reference.
- [docs/troubleshooting.md](docs/troubleshooting.md) covers debug logging and the things that go wrong.
- [docs/README.md](docs/README.md) indexes the rest, including the developer docs.

## License

MIT. See [LICENSE](LICENSE).

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability privately.

## Contributing

Bug reports and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).
