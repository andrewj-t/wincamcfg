# wincamcfg documentation

## Using wincamcfg

- [usage.md](usage.md): how to do the common tasks, from fixing flicker to running `set` from a startup script.
- [properties.md](properties.md): every property, the values it accepts and whether it supports Auto.
- [reference.md](reference.md): every subcommand and flag, value syntax, exit codes, JSON shapes and `RUST_LOG` levels.
- [troubleshooting.md](troubleshooting.md): trace logging, missing cameras, unsupported properties and writes that do not stick.
- [release-verification.md](release-verification.md): verifying the attestations and SBOMs that ship with each release.

## Working on wincamcfg

- [development.md](development.md): build, test and lint commands, code layout, what only a real camera can tell you, CI and releases.
- [architecture.md](architecture.md): diagrams of the modules, the device model and what `set` does step by step.
