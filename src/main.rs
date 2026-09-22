//! wincamcfg: command-line control of webcam properties on Windows.
//!
//! All DirectShow work lives in [`webcam`]; this file parses arguments,
//! formats text or JSON output and maps results to exit codes. Diagnostics go
//! to stderr through `tracing`, controlled by `RUST_LOG` (default `warn`).

mod webcam;

use std::fmt::Write as _;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use indexmap::IndexMap;
use tracing::{debug, info};
use tracing_subscriber::filter::LevelFilter;

use webcam::{ComSession, ParsedValue, Persistence, Property, PropertyInfo, WriteOutcome, Written};

/// Exit code for usage errors and failed enumeration.
const EXIT_ERROR: u8 = 1;
/// Exit code when `set` ran but one or more writes failed.
const EXIT_PARTIAL_FAILURE: u8 = 2;

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

/// A command-line utility for managing webcam properties.
#[derive(Debug, Parser)]
#[command(name = "wincamcfg", version)]
#[command(about = "Manage webcam properties")]
#[command(
    long_about = "A command line utility for managing webcam configuration on windows.\n\nConfigure camera properties like brightness, contrast, focus, exposure, and more using DirectShow APIs."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// List all video capture devices
    List {
        /// Include device path in output
        #[arg(long)]
        include_device_path: bool,

        /// Output format
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },

    /// Get property values from camera(s)
    Get {
        /// Camera index from list command (0-based), or "all" for all cameras
        #[arg(short, long)]
        camera: String,

        /// Output format
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },

    /// Set a property value on camera(s)
    #[command(group = ArgGroup::new("target").required(true))]
    Set {
        /// Camera index from list command (0-based), or "all" for all cameras
        #[arg(short, long)]
        camera: String,

        /// Property to set (e.g., PowerlineFrequency, Brightness, Contrast), or "all" to reset all properties (requires --default)
        #[arg(short, long)]
        property: String,

        /// Value to set (a number, a label such as 50Hz/On/Off, or Auto)
        #[arg(short, long, group = "target", allow_hyphen_values = true)]
        value: Option<String>,

        /// Set to the device's default value
        #[arg(short, long, group = "target")]
        default: bool,

        /// If the driver only stored a value for the next device start, restart the
        /// camera now so it takes effect (needs an elevated prompt; interrupts
        /// applications using the camera)
        #[arg(long)]
        restart_device: bool,

        /// Output format
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },

    /// Open the driver's own property dialog for one camera
    Dialog {
        /// Camera index from list command (0-based)
        #[arg(short, long)]
        camera: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

// ---------------------------------------------------------------------------
// Output structures
// ---------------------------------------------------------------------------

/// One device with its formatted properties, for `get`.
#[derive(Debug, serde::Serialize)]
struct DeviceOutput<'a> {
    index: usize,
    name: &'a str,
    properties: IndexMap<String, PropertyOutput>,
}

/// One property with every value already formatted for display.
#[derive(Debug, serde::Serialize)]
struct PropertyOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    default: String,
    min: i32,
    max: i32,
    step: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    supported_values: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modes_supported: Option<String>,
}

/// Outcome of one property write, for `set`.
#[derive(Debug, serde::Serialize)]
struct SetResult {
    index: usize,
    name: String,
    property: String,
    value: String,
    success: bool,
    /// Set when the write succeeded with a caveat the caller should know about.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let outcome = run(cli, &mut out)
        .and_then(|code| out.flush().context("Failed to write output").map(|()| code));
    match outcome {
        Ok(code) => code,
        // The reader went away (e.g. `| head`); there is nothing left to say.
        Err(e) if is_broken_pipe(&e) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

/// Installs the stderr log subscriber, honouring `RUST_LOG` as a single level.
fn init_tracing() {
    let level = match std::env::var("RUST_LOG") {
        Ok(value) => value.parse::<LevelFilter>().unwrap_or_else(|_| {
            eprintln!(
                "warning: RUST_LOG='{value}' is not a log level (trace|debug|info|warn|error|off); using warn"
            );
            LevelFilter::WARN
        }),
        Err(_) => LevelFilter::WARN,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(io::stderr)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .init();
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| matches!(cause.downcast_ref::<io::Error>(), Some(io) if io.kind() == io::ErrorKind::BrokenPipe))
}

fn run(cli: Cli, out: &mut dyn Write) -> Result<ExitCode> {
    debug!(args = ?std::env::args().collect::<Vec<_>>(), "Command line");
    match cli.command {
        Commands::List {
            include_device_path,
            output,
        } => list_devices(include_device_path, output, out)?,
        Commands::Get { camera, output } => get_device_properties(&camera, output, out)?,
        Commands::Dialog { camera } => open_dialog(&camera, out)?,
        Commands::Set {
            camera,
            property,
            value,
            restart_device,
            output,
            ..
        } => {
            return set_property(
                &camera,
                &property,
                value.as_deref(),
                restart_device,
                output,
                out,
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn list_devices(
    include_device_path: bool,
    output: OutputFormat,
    out: &mut dyn Write,
) -> Result<()> {
    debug!(include_device_path, ?output, "Listing devices");
    let com = ComSession::new()?;
    let devices = webcam::list_devices(&com).context("Failed to enumerate devices")?;
    info!(count = devices.len(), "Devices found");
    match output {
        OutputFormat::Json => writeln!(out, "{}", render_json(&devices)?)?,
        OutputFormat::Text => {
            if devices.is_empty() {
                writeln!(out, "No video capture devices found.")?;
            }
            for device in &devices {
                let suffix = device
                    .device_path
                    .as_deref()
                    .filter(|_| include_device_path)
                    .map_or_else(String::new, |path| format!(" ({path})"));
                writeln!(out, "[{}] {}{suffix}", device.index, device.name)?;
            }
        }
    }
    Ok(())
}

fn get_device_properties(camera: &str, output: OutputFormat, out: &mut dyn Write) -> Result<()> {
    debug!(camera, ?output, "Getting device properties");
    let com = ComSession::new()?;
    let devices = webcam::open_devices(&com).context("Failed to enumerate devices")?;
    let indices = parse_camera_selection(camera, devices.len())?;
    let outputs: Vec<DeviceOutput> = indices
        .iter()
        .map(|&idx| build_device_output(idx, &devices[idx].info))
        .collect();
    match output {
        OutputFormat::Text => render_text(&outputs, out)?,
        OutputFormat::Json => writeln!(out, "{}", render_json(&outputs)?)?,
    }
    Ok(())
}

fn open_dialog(camera: &str, out: &mut dyn Write) -> Result<()> {
    debug!(camera, "Opening property dialog");
    if camera.eq_ignore_ascii_case("all") {
        bail!("The dialog can only be opened for one camera; pass its index");
    }
    let com = ComSession::new()?;
    let devices = webcam::open_devices(&com).context("Failed to enumerate devices")?;
    let idx = parse_camera_selection(camera, devices.len())?[0];
    let device = &devices[idx];
    writeln!(
        out,
        "[{idx}] {}: opening property dialog...",
        device.info.name
    )?;
    out.flush()?;
    device.open_property_dialog()?;
    writeln!(out, "[{idx}] {}: dialog closed", device.info.name)?;
    Ok(())
}

/// Runs `set`; `value` is `None` for `--default` (clap guarantees exactly one of the two).
fn set_property(
    camera: &str,
    property: &str,
    value: Option<&str>,
    restart_device: bool,
    output: OutputFormat,
    out: &mut dyn Write,
) -> Result<ExitCode> {
    debug!(
        camera,
        property,
        ?value,
        restart_device,
        ?output,
        "Setting property"
    );
    let select_all = camera.eq_ignore_ascii_case("all");
    let reset_all = property.eq_ignore_ascii_case("all");
    if reset_all && value.is_some() {
        bail!("Property 'all' can only be used with --default");
    }
    if restart_device && !webcam::is_elevated() {
        bail!("--restart-device needs administrator rights; run this from an elevated prompt");
    }

    let com = ComSession::new()?;
    let devices = webcam::open_devices(&com).context("Failed to enumerate devices")?;
    let indices = parse_camera_selection(camera, devices.len())?;

    // Resolve the property name and parse the value once, up front: a typo is
    // a usage error (exit 1), not a per-device failure.
    let request: Option<(Property, ParsedValue)> = if reset_all {
        None
    } else {
        let property: Property = property.parse()?;
        let value = match value {
            None => ParsedValue::Default,
            Some(text) => webcam::parse_property_value(property, text)?,
        };
        Some((property, value))
    };

    let mut results: Vec<SetResult> = Vec::new();
    for idx in indices {
        let device = &devices[idx];
        let info = &device.info;
        let device_name = info.name.as_str();
        let jobs: Vec<(&PropertyInfo, ParsedValue)> = match request {
            None => info
                .properties
                .iter()
                .map(|p| (p, ParsedValue::Default))
                .collect(),
            Some((property, value)) => match info.property(property) {
                Some(p) => vec![(p, value)],
                // With `--camera all`, devices that lack the property are
                // skipped so one virtual camera cannot fail a fleet-wide set.
                None if select_all => {
                    info!(device_index = idx, device_name, %property, "Property not supported; skipped");
                    if output == OutputFormat::Text {
                        writeln!(
                            out,
                            "[{idx}] {device_name}: {property} not supported (skipped)"
                        )?;
                    }
                    Vec::new()
                }
                None => bail!("Property '{property}' not found on device '{device_name}'"),
            },
        };

        let entries: Vec<SetResult> = device
            .write_all(&jobs, restart_device)?
            .into_iter()
            .zip(&jobs)
            .map(|(outcome, &(prop, value))| {
                set_result(idx, device_name, prop.property, value, outcome)
            })
            .collect();
        if output == OutputFormat::Text {
            for r in &entries {
                let line = match (&r.error, &r.note) {
                    (Some(error), _) => format!("Failed to set {} - {error}", r.property),
                    (None, Some(note)) => format!("{} set to {} ({note})", r.property, r.value),
                    (None, None) => format!("{} set to {}", r.property, r.value),
                };
                writeln!(out, "[{idx}] {device_name}: {line}")?;
            }
        }
        results.extend(entries);
    }

    if output == OutputFormat::Json {
        writeln!(out, "{}", render_json(&results)?)?;
    }
    Ok(if results.iter().all(|r| r.success) {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_PARTIAL_FAILURE)
    })
}

/// Turns one write outcome into a result row, with the user-facing wording.
fn set_result(
    idx: usize,
    device_name: &str,
    property: Property,
    requested: ParsedValue,
    outcome: WriteOutcome,
) -> SetResult {
    let mut row = SetResult {
        index: idx,
        name: device_name.to_owned(),
        property: property.to_string(),
        value: String::new(),
        success: true,
        note: None,
        error: None,
    };
    match outcome {
        Err(error) => {
            row.value = display_requested(property, requested);
            row.error = Some(format!("{error:#}"));
        }
        Ok(report) => {
            row.value = display_written(property, report.written);
            let now = |current: webcam::CurrentValue| display_current(property, current);
            match (report.persistence, report.restarted) {
                (Persistence::Applied | Persistence::Unverified, false) => {}
                (Persistence::Applied, true) => {
                    row.note = Some("applied after restarting the device".to_owned());
                }
                (Persistence::Unverified, true) => {
                    row.note =
                        Some("device restarted; the value could not be read back".to_owned());
                }
                (Persistence::Stored(current), _) => {
                    row.note = Some(format!(
                        "stored by the driver and applied when the camera next starts (reconnect it or reboot); until then the device reports {} except while an application has it open",
                        now(current)
                    ));
                }
                (Persistence::Dropped(current), false) => {
                    row.error = Some(format!(
                        "the driver accepted the write but the device now reports {}; this camera may keep the setting only while an application has it open",
                        now(current)
                    ));
                }
                (Persistence::Dropped(current), true) => {
                    row.error = Some(format!(
                        "the device was restarted but still reports {}",
                        now(current)
                    ));
                }
            }
        }
    }
    row.success = row.error.is_none();
    row
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolves `--camera` to device indices: `all`, or one 0-based index.
fn parse_camera_selection(camera: &str, device_count: usize) -> Result<Vec<usize>> {
    if camera.eq_ignore_ascii_case("all") {
        return Ok((0..device_count).collect());
    }
    let idx: usize = camera
        .parse()
        .with_context(|| format!("Invalid camera index '{camera}': must be a number or 'all'"))?;
    if idx >= device_count {
        bail!("Camera index {idx} not found (only {device_count} devices available)");
    }
    Ok(vec![idx])
}

/// What was actually sent to the driver, e.g. `50Hz` or `4000 [Auto]`.
fn display_written(property: Property, written: Written) -> String {
    let value = webcam::format_property_value(property, written.value);
    match written.mode {
        webcam::Mode::Manual => value,
        webcam::Mode::Auto => format!("{value} [Auto]"),
    }
}

/// What the user asked for, used when the write itself failed.
fn display_requested(property: Property, value: ParsedValue) -> String {
    match value {
        ParsedValue::Auto => "Auto".to_owned(),
        ParsedValue::Default => "default".to_owned(),
        ParsedValue::Manual(v) => webcam::format_property_value(property, v),
    }
}

/// What the device reports now, e.g. `50Hz` or `3534 [Auto]`.
fn display_current(property: Property, current: webcam::CurrentValue) -> String {
    let value = webcam::format_property_value(property, current.value);
    if current.is_auto() {
        format!("{value} [Auto]")
    } else {
        value
    }
}

/// Converts a device's property list into display-ready output.
fn build_device_output(idx: usize, device: &webcam::DeviceInfo) -> DeviceOutput<'_> {
    let properties = device
        .properties
        .iter()
        .map(|prop| {
            let property = prop.property;
            (
                property.to_string(),
                PropertyOutput {
                    value: prop
                        .current
                        .map(|c| webcam::format_property_value(property, c.value)),
                    mode: prop
                        .current
                        .and_then(|c| webcam::current_mode(prop.caps, c))
                        .map(|m| m.to_string()),
                    default: webcam::format_property_value(property, prop.default),
                    min: prop.min,
                    max: prop.max,
                    step: prop.step,
                    supported_values: webcam::build_enum_display(property, prop.min, prop.max),
                    modes_supported: webcam::format_capabilities(prop.caps),
                },
            )
        })
        .collect();
    DeviceOutput {
        index: idx,
        name: &device.name,
        properties,
    }
}

fn render_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("Failed to serialize to JSON")
}

fn render_text(outputs: &[DeviceOutput], out: &mut dyn Write) -> Result<()> {
    for output in outputs {
        writeln!(out, "[{}] {}", output.index, output.name)?;
        writeln!(out, "  Properties:")?;
        if output.properties.is_empty() {
            writeln!(out, "    No properties available")?;
        }
        for (name, prop) in &output.properties {
            writeln!(out, "    {name}: {}", format_property_line(prop))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

/// Formats one property as `value [mode] (Range: ..., Modes: ..., Default: ...)`.
///
/// Enum-like properties list their labels under `Supported:` instead of a
/// range, like the drop-down versus slider split in the standard dialog.
fn format_property_line(prop: &PropertyOutput) -> String {
    let Some(current) = &prop.value else {
        return "<unavailable>".to_owned();
    };
    let mut line = current.clone();
    if let Some(mode) = &prop.mode {
        // Writing to a String cannot fail.
        let _ = write!(line, " [{mode}]");
    }
    let mut meta = Vec::with_capacity(4);
    if let Some(supported) = &prop.supported_values {
        meta.push(format!("Supported: {supported}"));
    } else {
        let mut range = format!("Range: {}..{}", prop.min, prop.max);
        if prop.step > 1 {
            let _ = write!(range, " step {}", prop.step);
        }
        meta.push(range);
    }
    if let Some(modes) = &prop.modes_supported
        && modes.contains(',')
    {
        meta.push(format!("Modes: {modes}"));
    }
    meta.push(format!("Default: {}", prop.default));
    let _ = write!(line, " ({})", meta.join(", "));
    line
}

// ---------------------------------------------------------------------------
// Tests (no hardware or COM required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::error::ErrorKind;
    use webcam::{CurrentValue, Mode, WriteReport};

    fn parse(command_line: &str) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(command_line.split(' '))
    }

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn camera_selection_accepts_all_and_indices() {
        assert_eq!(parse_camera_selection("all", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_camera_selection("ALL", 2).unwrap(), vec![0, 1]);
        assert_eq!(
            parse_camera_selection("all", 0).unwrap(),
            Vec::<usize>::new()
        );
        assert_eq!(parse_camera_selection("1", 3).unwrap(), vec![1]);
    }

    #[test]
    fn camera_selection_rejects_bad_input() {
        for camera in ["3", "-1", "a", "", "99999999999999999999"] {
            parse_camera_selection(camera, 3).unwrap_err();
        }
    }

    #[test]
    fn set_accepts_negative_values() {
        let cli = parse("wincamcfg set -c 0 -p Exposure --value -5").unwrap();
        let Commands::Set { value, default, .. } = cli.command else {
            panic!("unexpected command {:?}", cli.command);
        };
        assert_eq!(value.as_deref(), Some("-5"));
        assert!(!default);
    }

    #[test]
    fn set_requires_exactly_one_of_value_and_default() {
        let conflict = parse("wincamcfg set -c 0 -p Brightness --value 1 --default").unwrap_err();
        assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
        let missing = parse("wincamcfg set -c 0 -p Brightness").unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
        let cli = parse("wincamcfg set -c all -p all --default").unwrap();
        assert!(matches!(
            cli.command,
            Commands::Set {
                default: true,
                value: None,
                ..
            }
        ));
    }

    #[test]
    fn restart_device_flag_parses() {
        let cli = parse("wincamcfg set -c 0 -p PowerlineFrequency --value 50Hz --restart-device")
            .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Set {
                restart_device: true,
                ..
            }
        ));
        let cli = parse("wincamcfg set -c 0 -p Brightness --default").unwrap();
        assert!(matches!(
            cli.command,
            Commands::Set {
                restart_device: false,
                ..
            }
        ));
    }

    #[test]
    fn version_flag_is_available() {
        let err = parse("wincamcfg --version").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        assert!(err.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn display_values_use_labels_and_modes() {
        let written = |value, mode| Written { value, mode };
        let current = |value, flags| CurrentValue { value, flags };
        assert_eq!(
            display_written(Property::PowerlineFrequency, written(1, Mode::Manual)),
            "50Hz"
        );
        assert_eq!(
            display_written(Property::Brightness, written(128, Mode::Manual)),
            "128"
        );
        assert_eq!(
            display_written(Property::WhiteBalance, written(4000, Mode::Auto)),
            "4000 [Auto]"
        );
        assert_eq!(
            display_requested(Property::Focus, ParsedValue::Auto),
            "Auto"
        );
        assert_eq!(
            display_requested(Property::Focus, ParsedValue::Default),
            "default"
        );
        assert_eq!(
            display_current(Property::Exposure, current(-6, Mode::Auto.flag())),
            "-6 [Auto]"
        );
        assert_eq!(
            display_current(Property::PowerlineFrequency, current(1, 0)),
            "50Hz"
        );
    }

    #[test]
    fn set_results_follow_persistence_and_restart() {
        let written = Written {
            value: 1,
            mode: Mode::Manual,
        };
        let current = CurrentValue { value: 2, flags: 0 };
        let row = |persistence, restarted| {
            let report = WriteReport {
                written,
                persistence,
                restarted,
            };
            set_result(
                0,
                "Cam",
                Property::PowerlineFrequency,
                ParsedValue::Manual(1),
                Ok(report),
            )
        };
        let text = |s: &Option<String>| s.clone().unwrap_or_default();

        let applied = row(Persistence::Applied, false);
        assert!(applied.success && applied.note.is_none() && applied.error.is_none());
        assert_eq!(applied.value, "50Hz");
        let unverified = row(Persistence::Unverified, false);
        assert!(unverified.success && unverified.note.is_none());
        let stored = row(Persistence::Stored(current), false);
        assert!(stored.success && text(&stored.note).contains("reports 60Hz"));
        let dropped = row(Persistence::Dropped(current), false);
        assert!(!dropped.success && text(&dropped.error).contains("now reports 60Hz"));
        let restarted = row(Persistence::Applied, true);
        assert!(restarted.success);
        assert_eq!(text(&restarted.note), "applied after restarting the device");
        let still_dropped = row(Persistence::Dropped(current), true);
        assert!(
            !still_dropped.success && text(&still_dropped.error).contains("still reports 60Hz")
        );
        let unread = row(Persistence::Unverified, true);
        assert!(unread.success && text(&unread.note).contains("could not be read back"));

        let rejected = set_result(
            0,
            "Cam",
            Property::PowerlineFrequency,
            ParsedValue::Manual(1),
            Err(anyhow::anyhow!("driver said no")),
        );
        assert!(!rejected.success);
        assert_eq!(rejected.value, "50Hz");
        assert_eq!(text(&rejected.error), "driver said no");
    }

    fn output(
        value: Option<&str>,
        min: i32,
        max: i32,
        step: i32,
        supported: Option<&str>,
    ) -> PropertyOutput {
        PropertyOutput {
            value: value.map(str::to_owned),
            mode: None,
            default: "1".to_owned(),
            min,
            max,
            step,
            supported_values: supported.map(str::to_owned),
            modes_supported: None,
        }
    }

    #[test]
    fn property_line_shows_numeric_range_and_step() {
        assert_eq!(
            format_property_line(&output(Some("128"), 0, 255, 1, None)),
            "128 (Range: 0..255, Default: 1)"
        );
        assert_eq!(
            format_property_line(&output(Some("100"), 100, 500, 10, None)),
            "100 (Range: 100..500 step 10, Default: 1)"
        );
        assert_eq!(
            format_property_line(&output(Some("-5"), -11, -1, 1, None)),
            "-5 (Range: -11..-1, Default: 1)"
        );
    }

    #[test]
    fn property_line_prefers_labels_over_range_for_enum_properties() {
        let line = format_property_line(&output(Some("50Hz"), 1, 2, 1, Some("50Hz (1), 60Hz (2)")));
        assert_eq!(line, "50Hz (Supported: 50Hz (1), 60Hz (2), Default: 1)");
        assert!(!line.contains("Range"));
    }

    #[test]
    fn property_line_marks_unreadable_values() {
        assert_eq!(
            format_property_line(&output(None, 0, 255, 1, None)),
            "<unavailable>"
        );
    }

    #[test]
    fn broken_pipe_is_detected_through_context() {
        let err =
            anyhow::Error::from(io::Error::from(io::ErrorKind::BrokenPipe)).context("writing");
        assert!(is_broken_pipe(&err));
        assert!(!is_broken_pipe(&anyhow::anyhow!("something else")));
    }
}
