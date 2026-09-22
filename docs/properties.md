# Properties

Which properties a camera supports, and which of them can run in Auto mode, is decided by the device and its driver, not by `wincamcfg`. The table below lists every property the tool knows about, in the order `get` prints them. Run `wincamcfg get --camera 0` to see what your own camera reports: properties missing from that output are not supported by the device, and a `Modes:` entry appears only for properties that can switch between Manual and Auto.

The last two columns are an example, measured on a Logitech HD Pro Webcam C920. Other cameras report different ranges and defaults, and support a different subset.

| Property | Interface | Value | Labels | Auto on the C920 | C920 range and default |
|---|---|---|---|---|---|
| `Brightness` | VideoProcAmp | number | | no | 0..255, default 128 |
| `Contrast` | VideoProcAmp | number | | no | 0..255, default 128 |
| `Hue` | VideoProcAmp | number | | not supported | not supported |
| `Saturation` | VideoProcAmp | number | | no | 0..255, default 128 |
| `Sharpness` | VideoProcAmp | number | | no | 0..255, default 128 |
| `Gamma` | VideoProcAmp | number | | not supported | not supported |
| `WhiteBalance` | VideoProcAmp | number | | yes | 2000..6500, default 4000 |
| `BacklightCompensation` | VideoProcAmp | labels | `Off` (0), `On` (1) | no | `Off`..`On`, default `Off` |
| `Gain` | VideoProcAmp | number | | no | 0..255, default 0 |
| `ColorEnable` | VideoProcAmp | labels | `Off` (0), `On` (1) | not supported | not supported |
| `PowerlineFrequency` | VideoProcAmp | labels | `Disabled` (0), `50Hz` (1), `60Hz` (2), `Auto` (3) | no modes reported | `50Hz`..`60Hz`, default `60Hz` |
| `WhiteBalanceComponent` | VideoProcAmp | number | | not supported | not supported |
| `DigitalMultiplier` | VideoProcAmp | number | | not supported | not supported |
| `DigitalMultiplierLimit` | VideoProcAmp | number | | not supported | not supported |
| `Zoom` | CameraControl | number | | no | 100..500, default 100 |
| `Focus` | CameraControl | number | | yes | 0..250 step 5, default 0 |
| `Exposure` | CameraControl | number | | yes | -11..-2, default -5 |
| `Iris` | CameraControl | number | | not supported | not supported |
| `Pan` | CameraControl | number | | no | -10..10, default 0 |
| `Tilt` | CameraControl | number | | no | -10..10, default 0 |
| `Roll` | CameraControl | number | | not supported | not supported |

"not supported" means the C920 does not report the property at all, so `get` omits it and `set` fails with `Property not found on device`. With `--camera all` such a device is skipped with a notice instead.

The interface column is the DirectShow interface the property belongs to, `IAMVideoProcAmp` or `IAMCameraControl`. It matters only if you are reading the code; the command line treats both the same.

Property names are matched case-insensitively.

## Auto and Manual

A property that reports Auto support can be driven either way:

- `--value Auto` hands control to the driver. The current value stays as a starting point and the driver takes over from there.
- `--value <number>` switches the property to manual mode at that value. That is how you turn auto exposure or autofocus off.
- `--default` restores the driver's default value and re-enables Auto where the property supports it, the same as the Default button in the Windows property dialog.

Asking for `Auto` on a property that only supports Manual is an error, and so is a manual value on a property that only supports Auto. `get` shows what each property allows under `Modes:`.

`PowerlineFrequency` is a special case: `Auto` is one of its labels, so `--value Auto` selects that label (value 3) rather than switching the property into Auto mode. See [reference.md](reference.md#values).
