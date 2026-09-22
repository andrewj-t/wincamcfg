# Usage

Task-oriented walkthrough of what `wincamcfg` is for. [reference.md](reference.md) lists every flag, and [properties.md](properties.md) lists every property.

## List all cameras

See what cameras are connected to your system:

```bash
wincamcfg list
```

Example output:

```text
[0] HD Pro Webcam C920
[1] OBS Virtual Camera
```

The number in brackets is the camera index every other command takes as `--camera`.

## Get current settings

Check current property values for a specific camera:

```bash
# Get all properties for camera 0
wincamcfg get --camera 0

# Get all properties for all cameras
wincamcfg get --camera all

# Output as JSON for scripting
wincamcfg get --camera 0 --output json
```

`get` is the place to start with an unfamiliar camera: it shows only the properties that camera actually supports, with the range, the default, the current value and, for properties that can switch modes, a `Modes:` list and the current mode in square brackets. Above the properties it prints a `Driver:` block with the description, manufacturer, provider, version, date and INF file Windows recorded for the device, the same details Device Manager shows. Virtual cameras have no driver block.

## Fix powerline-frequency flickering

The main reason this tool exists. Set your cameras to match your local power grid:

```bash
# Set camera 0 to 50Hz (for most of Europe, Asia, Africa, Australia)
wincamcfg set --camera 0 --property PowerlineFrequency --value 50Hz

# Set camera 0 to 60Hz (for Americas, parts of Asia)
wincamcfg set --camera 0 --property PowerlineFrequency --value 60Hz

# Set ALL cameras to 50Hz
wincamcfg set --camera all --property PowerlineFrequency --value 50Hz
```

Every write is read back through a fresh handle, so `set` reports whether the camera kept the value, only stored it for its next start, or dropped it (exit code 2). [troubleshooting.md](troubleshooting.md) explains each outcome, and `--restart-device` below applies a stored value straight away.

## Adjust other properties

Other settings you can change:

```bash
# Adjust brightness
wincamcfg set --camera 0 --property Brightness --value 128

# Adjust contrast
wincamcfg set --camera 0 --property Contrast --value 150

# Set a fixed white balance temperature
wincamcfg set --camera 0 --property WhiteBalance --value 4000

# Disable backlight compensation
wincamcfg set --camera 0 --property BacklightCompensation --value Off
```

Property names are matched case-insensitively, so `brightness` and `Brightness` are the same property. Run `wincamcfg get --camera 0` first to see the range your camera accepts; a value outside it is rejected.

## Switch a property to Auto, or back to Manual

Properties such as `Exposure`, `Focus` and `WhiteBalance` can run in either Auto or Manual mode, if the camera says so. `get` shows `Modes: Manual, Auto` for those, and the current mode in square brackets, e.g. `Exposure: -5 [Manual]` or `Exposure: -5 [Auto]`.

To hand a property to the driver, pass `Auto`:

```bash
# Auto exposure on
wincamcfg set --camera 0 --property Exposure --value Auto

# Autofocus on
wincamcfg set --camera 0 --property Focus --value Auto
```

To take it back, pass a number. Any number switches the property to manual mode at that value, so the way to turn auto exposure off is to set an explicit exposure:

```bash
# Auto exposure off, fixed at -5 (the C920 range is -11..-2; check yours with get)
wincamcfg set --camera 0 --property Exposure --value -5

# Autofocus off, fixed focus at 0 (the C920 range is 0..250 in steps of 5)
wincamcfg set --camera 0 --property Focus --value 0
```

`PowerlineFrequency` is the exception: `Auto` there is one of the labels the property accepts, not a mode switch, so `--value Auto` selects the driver's "Auto" frequency setting where the camera offers it.

## Reset to defaults

Restore factory settings. Properties that support Auto go back to Auto, as the Default button in the Windows dialog does:

```bash
# Reset a specific property to default
wincamcfg set --camera 0 --property Brightness --default

# Reset ALL properties on a camera to defaults
wincamcfg set --camera 0 --property all --default

# Reset ALL cameras to factory defaults
wincamcfg set --camera all --property all --default
```

With `--camera all`, a device that does not support the requested property (a virtual camera, say) is skipped with a notice; with a specific index it is an error.

## Open the driver's own dialog

To see exactly what Windows shows for a camera (the same "Video Proc Amp" and "Camera Control" pages OBS Studio opens under *Configure Video*), open the driver's property dialog for one camera:

```bash
wincamcfg dialog --camera 0
```

The command blocks until the dialog is closed. Changes made there are written by the driver's own page; use `get` afterwards to confirm them.

## Run it from a script

Use `--output json` for machine-readable output and check the exit code:

```powershell
# PowerShell example: configure all cameras on startup
wincamcfg set --camera all --property PowerlineFrequency --value 50Hz --output json
if ($LASTEXITCODE -ne 0) { Write-Error "camera configuration failed" }
```

Exit code 0 means success, 1 a usage error or a failed enumeration, and 2 that `set` ran but at least one write failed. Diagnostics go to stderr, so `--output json` on stdout stays parseable even with `RUST_LOG` set. [reference.md](reference.md#exit-codes) has the full table and the JSON shapes.

Drop this into a startup script or GPO if you need every machine on a fleet to land on the same camera config.

## Apply a stored value immediately with `--restart-device`

Some cameras revert a written value as soon as the last handle closes, and the Windows UVC class driver stores it for the next device start instead. `set` reports that case as success with a note. From an elevated prompt (which is how startup scripts and GPO usually run), add `--restart-device` to have `set` restart the camera so the value takes effect now:

```powershell
wincamcfg set --camera 0 --property PowerlineFrequency --value 50Hz --restart-device
```

The restart takes a second or two and interrupts any application currently using that camera, which is why it is opt-in. Cameras that apply the value straight away are never restarted. Without administrator rights the flag is refused up front with exit code 1.
