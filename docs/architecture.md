# Architecture

How the pieces of wincamcfg fit together. The diagrams are Mermaid and render on GitHub; the source of truth is the code and its module docs, so if a diagram and the code disagree, the code wins.

## Modules

Five source files. `main.rs` parses the command line and picks a handler; `commands.rs` runs one subcommand; `output.rs` turns results into text or JSON; `webcam.rs` is the only module that calls Windows; `webcam/property.rs` is pure logic with no Windows imports.

```mermaid
flowchart LR
    main["main.rs<br/>clap types, entry point, exit codes"]
    commands["commands.rs<br/>list / get / set / dialog"]
    output["output.rs<br/>rows, text and JSON rendering"]
    webcam["webcam.rs<br/>COM session, devices, read-back, restart"]
    property["webcam/property.rs<br/>Property, Mode, labels, parsing"]
    windows[("windows / windows-registry crates")]

    main --> commands
    commands --> output
    commands --> webcam
    output --> webcam
    webcam --> property
    webcam --> windows
```

Arrows mean "uses". `webcam.rs` re-exports everything from `property.rs`, so the other modules refer to `webcam::Property` and never name the submodule.

## Device model (`webcam.rs` and `webcam/property.rs`)

`ComSession` is the proof that COM is initialised on this thread. Every `Device` borrows it, so the borrow checker guarantees no COM interface outlives the session. A `Device` holds the DirectShow moniker (the handle used to bind the driver) and a `DeviceInfo`, which is plain data. Each `PropertyInfo` describes one property the device reported, keyed by the `Property` enum. `DriverInfo` is what the registry says about the bound driver, read once at enumeration for devices that have a PnP path.

```mermaid
classDiagram
    direction LR

    class ComSession {
        +new() Result~ComSession~
    }

    class Device {
        -IMoniker moniker
        +DeviceInfo info
        +write_all(jobs, restart) Result~Vec~WriteOutcome~~
        +open_property_dialog() Result
        -set(info, value) Result~Written~
        -read_back(properties) Result~Vec~Option~CurrentValue~~~
        -stored_value(property) Option~i32~
        -restart() Result
    }

    class DeviceInfo {
        +String name
        +Option~String~ device_path
        +Vec~PropertyInfo~ properties
        +Option~DriverInfo~ driver
        +property(Property) Option~PropertyInfo~
    }

    class DriverInfo {
        +Option~String~ description
        +Option~String~ manufacturer
        +Option~String~ provider
        +Option~String~ version
        +Option~String~ date
        +Option~String~ inf_path
    }

    class PropertyInfo {
        +Property property
        +i32 min
        +i32 max
        +i32 step
        +i32 default
        +i32 caps
        +Option~CurrentValue~ current
    }

    class Property {
        <<enumeration>>
        Brightness .. PowerlineFrequency
        Pan .. Focus
        +kind() PropertyType
        +id() i32
        +as_str() str
    }

    class PropertyType {
        <<enumeration>>
        VideoProcAmp
        CameraControl
    }

    class CurrentValue {
        +i32 value
        +i32 flags
        +is_auto() bool
    }

    class Mode {
        <<enumeration>>
        Auto
        Manual
        +flag() i32
    }

    class ParsedValue {
        <<enumeration>>
        Auto
        Manual(i32)
        Default
    }

    class Written {
        +i32 value
        +Mode mode
        +persisted_in(CurrentValue) bool
    }

    class Persistence {
        <<enumeration>>
        Applied
        Stored(CurrentValue)
        Dropped(CurrentValue)
        Unverified
    }

    class WriteReport {
        +Written written
        +Persistence persistence
        +bool restarted
    }

    class PropertyControl {
        <<trait>>
        KIND PropertyType
        +range(Property) Result~PropertyInfo~
        +get(id) Result~CurrentValue~
        +set(id, value, flags) Result
    }

    class IAMVideoProcAmp
    class IAMCameraControl

    Device ..> ComSession : borrows for its lifetime
    Device *-- DeviceInfo
    DeviceInfo *-- "0..*" PropertyInfo
    DeviceInfo *-- "0..1" DriverInfo
    PropertyInfo --> Property
    PropertyInfo --> "0..1" CurrentValue
    Property --> PropertyType : kind()
    Written --> Mode
    WriteReport *-- Written
    WriteReport *-- Persistence
    Persistence ..> CurrentValue
    Device ..> ParsedValue : write_all input
    Device ..> WriteReport : write_all output
    PropertyControl <|.. IAMVideoProcAmp
    PropertyControl <|.. IAMCameraControl
    Device ..> PropertyControl : via bind_filter
```

`WriteOutcome` is `Result<WriteReport>`: an `Err` means the driver rejected the write, an `Ok` carries what was sent and what the read-back showed.

`PropertyControl` exists because `IAMVideoProcAmp` and `IAMCameraControl` have identical `GetRange`/`Get`/`Set` signatures. One macro implements it for both, and `Property::kind()` says which one to cast the bound filter to.

## Command-line and output types (`main.rs`, `commands.rs`, `output.rs`)

`Cli` and `Commands` are the clap derive types; `list`, `get` and `set` also take `--output`. `Request` is what `set` resolves the `--property` and `--value` arguments into before touching any device. The three output structs are serialised directly for `--output json` and rendered by hand for text; every value in them is already a formatted string, so both formats show the same labels.

```mermaid
classDiagram
    direction LR

    class Cli {
        +Commands command
    }

    class Commands {
        <<enumeration>>
        List(include_device_path)
        Get(camera)
        Set(camera, property, value, default, restart_device)
        Dialog(camera)
    }

    class OutputFormat {
        <<enumeration>>
        Text
        Json
    }

    class Request {
        <<enumeration>>
        ResetAll
        One(Property, ParsedValue)
    }

    class DeviceOutput {
        +usize index
        +String name
        +Option~String~ device_path
        +Option~DriverInfo~ driver
        +IndexMap~String, PropertyOutput~ properties
    }

    class PropertyOutput {
        +Option~String~ value
        +Option~String~ mode
        +String default
        +i32 min
        +i32 max
        +i32 step
        +Option~String~ supported_values
        +Option~String~ modes_supported
    }

    class SetResult {
        +usize index
        +String name
        +String property
        +String value
        +bool success
        +Option~String~ note
        +Option~String~ error
    }

    class DeviceListItem {
        +usize index
        +String name
        +Option~String~ device_path
    }

    Cli *-- Commands
    Commands ..> OutputFormat
    Commands ..> Request : set_property parses into
    DeviceOutput *-- "0..*" PropertyOutput
    PropertyOutput ..> PropertyInfo : From
    SetResult ..> WriteOutcome : set_result maps from
    DeviceListItem ..> DeviceInfo : list never binds a driver
```

## What `set` does

The interesting command. Everything above the `webcam` lane is plain Rust; everything below it is COM.

```mermaid
sequenceDiagram
    participant main as main.rs
    participant cmd as commands.rs
    participant wc as webcam.rs
    participant drv as DirectShow driver
    participant reg as Registry (usbvideo.sys)

    main->>cmd: set_property(camera, property, value, restart, output)
    cmd->>cmd: parse_request → Request
    cmd->>wc: is_elevated() (only with --restart-device)
    cmd->>wc: ComSession::new()
    cmd->>wc: open_devices(&com)
    wc->>drv: enumerate monikers, bind each filter, GetRange/Get every property
    wc->>reg: read driver details for each PnP device
    drv-->>wc: Vec#lt;Device#gt;
    loop each selected device
        cmd->>cmd: select jobs: all properties, or the one requested
        cmd->>wc: device.write_all(&jobs, restart)
        loop each job
            wc->>wc: resolve_set(info, value) → Written
            wc->>drv: bind filter, Set(id, value, flags)
        end
        wc->>drv: bind a fresh filter, Get each accepted property
        drv-->>wc: current values
        opt value reverted
            wc->>reg: stored_value(property)
            reg-->>wc: value the class driver stored, if any
        end
        opt restart requested and something was only stored
            wc->>drv: CM_Disable_DevNode, CM_Enable_DevNode
            wc->>drv: Get again, retrying until the device is back
        end
        wc-->>cmd: Vec#lt;WriteOutcome#gt;
        cmd->>cmd: set_result per outcome → SetResult rows
        cmd->>main: render rows (text now, JSON at the end)
    end
    cmd-->>main: all succeeded?
    main->>main: exit 0, or 2 if any write failed
```

## How a write is classified

After the driver accepts a write, `write_all` reads the property back through a fresh handle and decides what happened. This is the logic behind the notes and errors `set` prints, and behind the `--restart-device` flag.

```mermaid
flowchart TD
    accepted([Driver accepted the write]) --> readback{Fresh-handle read-back}
    readback -- "bind failed or driver refused" --> unverified[Unverified<br/>reported as success]
    readback -- "device reports the written value" --> applied[Applied<br/>success]
    readback -- "device reverted" --> stored{Class driver stored it<br/>in Device Parameters?}
    stored -- no --> dropped[Dropped<br/>failure, exit 2]
    stored -- yes --> storedok[Stored<br/>success with a note]
    storedok --> restart{"--restart-device?"}
    restart -- no --> done1([done])
    restart -- yes --> cycle[Disable and enable the device,<br/>read back again]
    cycle -- "reports the value" --> applied2[Applied after restart<br/>success with a note]
    cycle -- "still reverted" --> dropped2[Dropped after restart<br/>failure, exit 2]
    cycle -- "could not read" --> unverified2[Unverified after restart<br/>success with a note]
```

A `Stored` outcome cannot occur after a restart: at that point the stored-value check is skipped, because a value the device still does not report has been dropped.

## Where things are tested

`webcam/property.rs`, `output.rs` and `commands.rs` carry unit tests for parsing, formatting, `resolve_set`, the persistence check and the outcome-to-row mapping; `main.rs` tests the clap definitions. Nothing that touches COM has a unit test. `list`, `get` and `set` are checked by hand against a real camera, and the `dialog` subcommand shows the driver's own property pages as the reference for what correct looks like.
