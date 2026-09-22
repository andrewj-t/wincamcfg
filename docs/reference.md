# Reference

Every subcommand, flag, value form and output shape. For how to use them in practice see [usage.md](usage.md); for what each property means see [properties.md](properties.md).

## Commands

```text
A command line utility for managing webcam configuration on windows.

Configure camera properties like brightness, contrast, focus, exposure, and more using DirectShow APIs.

Usage: wincamcfg.exe <COMMAND>

Commands:
  list    List all video capture devices
  get     Get property values from camera(s)
  set     Set a property value on camera(s)
  dialog  Open the driver's own property dialog for one camera
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

### `list`

```text
List all video capture devices

Usage: wincamcfg.exe list [OPTIONS]

Options:
      --include-device-path  Include device path in output
  -o, --output <OUTPUT>      Output format [default: text] [possible values: text, json]
  -h, --help                 Print help
```

`list` reads only names and paths and never binds a driver, so a driver that stalls when bound cannot stall `list`.

### `get`

```text
Get property values from camera(s)

Usage: wincamcfg.exe get [OPTIONS] --camera <CAMERA>

Options:
  -c, --camera <CAMERA>  Camera index from list command (0-based), or "all" for all cameras
  -o, --output <OUTPUT>  Output format [default: text] [possible values: text, json]
  -h, --help             Print help
```

Text output shows the DirectShow device path under the device name and the JSON carries it as `device_path`, for every device that reports one.

### `set`

```text
Set a property value on camera(s)

Usage: wincamcfg.exe set [OPTIONS] --camera <CAMERA> --property <PROPERTY> <--value <VALUE>|--default>

Options:
  -c, --camera <CAMERA>      Camera index from list command (0-based), or "all" for all cameras
  -p, --property <PROPERTY>  Property to set (e.g., PowerlineFrequency, Brightness, Contrast), or "all" to reset all properties (requires --default)
  -v, --value <VALUE>        Value to set (a number, a label such as 50Hz/On/Off, or Auto)
  -d, --default              Set to the device's default value
      --restart-device       If the driver only stored a value for the next device start, restart the camera now so it takes effect (needs an elevated prompt; interrupts applications using the camera)
  -o, --output <OUTPUT>      Output format [default: text] [possible values: text, json]
  -h, --help                 Print help
```

`--value` and `--default` are mutually exclusive and one of them is required. `--property all` requires `--default`. With `--camera all`, a device that does not have the requested property is skipped with a notice instead of failing; with a specific index a missing property is an error.

### `dialog`

```text
Open the driver's own property dialog for one camera

Usage: wincamcfg.exe dialog --camera <CAMERA>

Options:
  -c, --camera <CAMERA>  Camera index from list command (0-based)
  -h, --help             Print help
```

`dialog` takes a single camera index; `all` is not accepted. The command blocks until the dialog is closed.

## Values

Property names are matched case-insensitively, so `--property powerlinefrequency` and `--property PowerlineFrequency` are the same.

### Numbers

Any decimal number within the property's reported range. Negative numbers work:

```bash
wincamcfg set --camera 0 --property Exposure --value -5
wincamcfg set --camera 0 --property Exposure --value=-5
```

Both forms above are accepted. A value outside `[min, max]` is rejected before anything is written. Where the driver reports a step larger than 1, an off-grid value is still sent and the driver may round it.

### Labels

Some properties are enumerations and accept labels instead of raw numbers. Labels are matched case-insensitively, and the number works too.

| Property | Labels (value) |
|---|---|
| `PowerlineFrequency` | `Disabled` (0), `50Hz` (1), `60Hz` (2), `Auto` (3) |
| `ColorEnable` | `Off` (0), `On` (1) |
| `BacklightCompensation` | `Off` (0), `On` (1) |

`get` prints the labels a device actually accepts as `Supported: 50Hz (1), 60Hz (2)`, clipped to the range the driver reported, so a camera that only offers 50Hz and 60Hz will reject `Disabled`.

### The `Auto` keyword

For any other property, `--value Auto` switches the property into Auto mode and hands the value to the driver. It is refused with an error if the property does not advertise Auto support.

`PowerlineFrequency` is the exception: the label table wins over the keyword, so `--value Auto` there selects the label `Auto` (value 3) in manual mode, not Auto mode.

### `--default`

`--default` writes the driver's default value. For properties that support Auto it also switches them back to Auto, which is what the Default button of the standard Windows property dialog does. To pin a property at its default value in manual mode instead, pass the number: `--value 4000`.

`--property all --default` resets every property the device supports.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Usage error, or the camera enumeration itself failed |
| 2 | `set` ran, but at least one property write failed (see the output for which) |

## JSON output

`--output json` is accepted by `list`, `get` and `set`. Diagnostics go to stderr, so stdout stays parseable even with `RUST_LOG` set.

### `list --output json`

An array of devices. `device_path` is included for every device that reports one; the `--include-device-path` flag only adds it to the text output.

```json
[
  {
    "index": 0,
    "name": "HD Pro Webcam C920",
    "device_path": "\\\\?\\usb#vid_046d&pid_082d&mi_00#6&1f335e1e&1&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\\global"
  },
  {
    "index": 1,
    "name": "OBS Virtual Camera"
  }
]
```

### `get --output json`

An array of devices, each with a `properties` object keyed by property name and in the same order as the text output. `device_path` is present for every device that reports one, and a `driver` object appears for devices Windows knows as PnP devices (a virtual camera has neither); each driver field appears only when the registry has it. `value` and `default` are already formatted, so an enumeration property shows its label. `mode` appears only for properties that can switch modes, `supported_values` only for properties with labels, and `modes_supported` only where the driver reported capabilities.

```json
[
  {
    "index": 0,
    "name": "HD Pro Webcam C920",
    "device_path": "\\\\?\\usb#vid_046d&pid_082d&mi_00#6&1f335e1e&1&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\\global",
    "driver": {
      "description": "HD Pro Webcam C920",
      "manufacturer": "Logitech",
      "provider": "Logitech",
      "version": "1.4.40.0",
      "date": "4-27-2021",
      "inf_path": "C:\\WINDOWS\\INF\\oem16.inf"
    },
    "properties": {
      "Brightness": {
        "value": "128",
        "default": "128",
        "min": 0,
        "max": 255,
        "step": 1,
        "modes_supported": "Manual"
      },
      "Exposure": {
        "value": "-5",
        "mode": "Auto",
        "default": "-5",
        "min": -11,
        "max": -2,
        "step": 1,
        "modes_supported": "Manual, Auto"
      },
      "PowerlineFrequency": {
        "value": "50Hz",
        "default": "60Hz",
        "min": 1,
        "max": 2,
        "step": 1,
        "supported_values": "50Hz (1), 60Hz (2)"
      }
    }
  }
]
```

### `set --output json`

One row per device and property written. `note` carries a message such as the stored-for-next-start explanation, and `error` the reason a write failed; each appears only when it applies.

```json
[
  {
    "index": 0,
    "name": "HD Pro Webcam C920",
    "property": "Brightness",
    "value": "128",
    "success": true
  }
]
```

## Logging

`RUST_LOG` takes one level for the whole program: `trace`, `debug`, `info`, `warn`, `error` or `off`. The default is `warn`, and an unrecognised value prints a warning and falls back to `warn`. There are no per-target filters. Logs go to stderr.

```powershell
$env:RUST_LOG="trace"; wincamcfg get --camera 0
```

[troubleshooting.md](troubleshooting.md) has more on capturing a log to a file.
