# Troubleshooting

## Enabling trace logging

`wincamcfg` uses Rust's `tracing` framework for logging. Set `RUST_LOG` to one of `trace`, `debug`, `info`, `warn`, `error`, or `off` to control verbosity. The default is `warn`.

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

Properties that show `value: None` or are missing entirely are not supported by your camera hardware.

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
