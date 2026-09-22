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

### A write is accepted but the value does not change

Some drivers acknowledge a write and then keep the old value, typically when the property is locked by the device firmware or by a vendor application (for example a Logitech tuning utility) that reapplies its own settings. Close such applications, re-run the command, and confirm with `get`. Trace logging shows the exact value and flags sent to the driver.

### "Access denied" or permission errors

Some cameras may be in use by another application. Close other programs that might be using the camera (video conferencing apps, camera apps, etc.) and try again.

## Reporting issues

When reporting an issue, please include:

1. Version, from `wincamcfg --version`
2. Camera model, from `wincamcfg get --camera <CAMERA>`
3. Trace log of the failing command:

   ```powershell
   $env:RUST_LOG="trace"; wincamcfg [your command] 2> debug.log
   ```

4. Windows version and anything else about the system that seems relevant

Attach the `debug.log` file to the issue.
