# Troubleshooting

## Enabling trace logging

`wincamcfg` uses Rust's `tracing` framework for logging. Set `RUST_LOG` to one of `trace`, `debug`, `info`, `warn`, `error`, or `off` to control verbosity. The default is `warn`, and an unrecognised value prints a warning and falls back to `warn`. Logs are written to stderr, so they never mix with `--output json` on stdout.

For a single command:

```powershell
$env:RUST_LOG="trace"; wincamcfg list
```

For an entire PowerShell session:

```powershell
$env:RUST_LOG="trace"
wincamcfg list
wincamcfg get --camera 0
# ... trace logs will be enabled for all commands in this session
```

### Saving logs to a file

```powershell
$env:RUST_LOG="trace"; wincamcfg list 2> debug.log
```

To capture both stdout and stderr:

```powershell
$env:RUST_LOG="trace"; wincamcfg list *> debug.log
```

## Common issues

### Camera not found

If `wincamcfg list` doesn't show your camera:

1. Verify the camera works in the Windows Camera app first
2. Check if the camera is visible in Windows Device Manager
3. Ensure the camera drivers are properly installed
4. Try unplugging and replugging the camera (for USB cameras)
5. Run with trace logging to see DirectShow enumeration details:

```powershell
$env:RUST_LOG="trace"; wincamcfg list
```

### Property not supported

Not all cameras support all properties. Use `get` to see which properties your camera supports:

```powershell
wincamcfg get --camera 0
```

Properties that are missing from the output are not supported by your camera. A property shown as `<unavailable>` is advertised by the driver but its current value could not be read.

### Exit code 2 after `set`

`set` exits with code 2 when at least one property write failed; the output names the property and the reason (for example a value outside the supported range, or a request for Auto mode on a property that only supports Manual). With `--camera all`, devices that do not have the property at all are skipped rather than counted as failures.

### "the driver accepted the write but the device now reports ..."

After every write, `wincamcfg` closes the camera, reopens it and reads the property back. Some cameras keep a written value only while an application has the camera open and revert it as soon as the last handle closes; the Logitech C920's PowerlineFrequency behaves this way (it keeps 50Hz in the device and drops a 60Hz write on close). The driver's own property dialog shows the same behaviour, it is just not visible there because the dialog keeps the camera open.

When you see this message the exit code is 2 and the setting has not stuck. Options:

1. Run the command while the application that uses the camera already has it open (a video call, OBS with the source active). The value then stays in effect for as long as that application holds the camera.
2. Change the setting with the vendor's own software, which may store it in the camera's non-volatile memory.

Trace logging (`RUST_LOG=trace`) shows the exact value and flags sent and the value read back.

### `--default` and Auto mode

`--default` restores the driver's default value and, for properties that support Auto, switches them back to Auto. This matches the Default button of the standard property dialog. To pin a property at its default value in manual mode instead, pass the value explicitly, e.g. `--value 4000`.
