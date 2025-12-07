# Troubleshooting

## Enabling Trace Logging

`wincamcfg` uses Rust's `tracing` framework for logging. You can enable detailed trace logs to diagnose issues by setting the `RUST_LOG` environment variable to `trace`.

**Enable trace logging for a single command:**

```powershell
$env:RUST_LOG="trace"; wincamcfg list
```

**Enable for specific modules only (wincamcfg code, not dependencies):**

```powershell
$env:RUST_LOG="wincamcfg=trace"; wincamcfg set --camera 0 --property PowerlineFrequency --value 50Hz
```

**Set for entire PowerShell session:**

```powershell
$env:RUST_LOG="trace"
wincamcfg list
wincamcfg get --camera 0
# ... trace logs will be enabled for all commands in this session
```

### Saving Logs to File

```powershell
$env:RUST_LOG="trace"; wincamcfg list 2> debug.log
```

**Or capture both stdout and stderr:**

```powershell
$env:RUST_LOG="trace"; wincamcfg list *> debug.log
```

## Common Issues

### Camera Not Found

If `wincamcfg list` doesn't show your camera:

1. Verify the camera works in the Windows Camera app first
2. Check if the camera is visible in Windows Device Manager
3. Ensure the camera drivers are properly installed
4. Try unplugging and replugging the camera (for USB cameras)
5. Run with trace logging to see DirectShow enumeration details:

```powershell
$env:RUST_LOG="trace"; wincamcfg list
```

### Property Not Supported

Not all cameras support all properties. Use `get` to see which properties your camera supports:

```powershell
wincamcfg get --camera 0
```

Properties that show `value: None` or are missing entirely are not supported by your camera hardware.

### "Access Denied" or Permission Errors

Some cameras may be in use by another application. Close other programs that might be using the camera (video conferencing apps, camera apps, etc.) and try again.

## Reporting Issues

When reporting issues, please include:

1. **Version information:**

   ```powershell
   wincamcfg --version
   ```

2. **Your camera model** (from `wincamcfg get --camera <CAMERA>`)

3. **Trace logs** showing the issue:

   ```powershell
   $env:RUST_LOG="trace"; wincamcfg [your command] 2> debug.log
   ```

4. **Windows version** and any relevant system information

You can then attach the `debug.log` file when reporting the issue.
