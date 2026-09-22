//! wincamcfg: command-line control of webcam properties on Windows.
//!
//! The binary has four subcommands (`list`, `get`, `set`, `version`) and two
//! output formats (text and JSON). All DirectShow work lives in [`webcam`];
//! this file only parses arguments, formats output and maps results to exit
//! codes.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0    | Success |
//! | 1    | Usage error, or the device enumeration itself failed |
//! | 2    | `set` completed but at least one property write failed |
//!
//! Diagnostics go to stderr through `tracing`; set `RUST_LOG` to one of
//! `trace`, `debug`, `info`, `warn`, `error` or `off` (default `warn`).

mod webcam;

use std::fmt::Write as _;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use indexmap::IndexMap;
use tracing::{debug, info};
use tracing_subscriber::filter::LevelFilter;

use webcam::{ComSession, Mode, ParsedValue, Written};

/// Exit code for usage errors and failed enumeration.
const EXIT_ERROR: u8 = 1;
/// Exit code when `set` ran but one or more writes failed.
const EXIT_PARTIAL_FAILURE: u8 = 2;

/// Longest accepted `--camera` argument: `all` or a device index.
///
/// Even an absurd number of cameras fits in far fewer digits; the cap simply
/// bounds what is parsed.
const MAX_CAMERA_SELECTOR_LEN: usize = 16;

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

    /// Show version information
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

/// What `set` should write: a user-supplied value or the device default.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SetValue {
    Explicit(String),
    Default,
}

/// Result of a command that can partially fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Success,
    PartialFailure,
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
    let outcome = run(cli, &mut out).and_then(|outcome| {
        out.flush()
            .context("Failed to write output")
            .map(|()| outcome)
    });

    match outcome {
        Ok(Outcome::Success) => ExitCode::SUCCESS,
        Ok(Outcome::PartialFailure) => ExitCode::from(EXIT_PARTIAL_FAILURE),
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

fn run(cli: Cli, out: &mut dyn Write) -> Result<Outcome> {
    debug!(args = ?std::env::args().collect::<Vec<_>>(), "Command line");

    match cli.command {
        Commands::List {
            include_device_path,
            output,
        } => list_devices(include_device_path, output, out)?,
        Commands::Get { camera, output } => get_device_properties(&camera, output, out)?,
        Commands::Dialog { camera } => open_dialog(&camera, out)?,
        Commands::Version => writeln!(out, "wincamcfg {}", env!("CARGO_PKG_VERSION"))?,
        Commands::Set {
            camera,
            property,
            value,
            restart_device,
            output,
            ..
        } => {
            // clap's `target` group guarantees exactly one of --value/--default.
            let target = value.map_or(SetValue::Default, SetValue::Explicit);
            if property.eq_ignore_ascii_case("all") && target != SetValue::Default {
                bail!("Property 'all' can only be used with --default");
            }
            if restart_device && !webcam::is_elevated() {
                bail!(
                    "--restart-device needs administrator rights; run this from an elevated prompt"
                );
            }
            return set_property(&camera, &property, &target, restart_device, output, out);
        }
    }

    Ok(Outcome::Success)
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

    // The session must outlive every COM object below, so it is created first.
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
                let suffix = match (&device.device_path, include_device_path) {
                    (Some(path), true) => format!(" ({path})"),
                    _ => String::new(),
                };
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
        .map(|&idx| build_device_output(idx, devices[idx].info()))
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
    let indices = parse_camera_selection(camera, devices.len())?;
    let idx = indices[0];
    let device = &devices[idx];

    writeln!(
        out,
        "[{idx}] {}: opening property dialog...",
        device.info().display_name()
    )?;
    out.flush()?;
    device.open_property_dialog()?;
    writeln!(
        out,
        "[{idx}] {}: dialog closed",
        device.info().display_name()
    )?;
    Ok(())
}

fn set_property(
    camera: &str,
    property: &str,
    target: &SetValue,
    restart_device: bool,
    output: OutputFormat,
    out: &mut dyn Write,
) -> Result<Outcome> {
    debug!(
        camera,
        property,
        ?target,
        restart_device,
        ?output,
        "Setting property"
    );

    let com = ComSession::new()?;
    let devices = webcam::open_devices(&com).context("Failed to enumerate devices")?;
    let indices = parse_camera_selection(camera, devices.len())?;
    let select_all = camera.eq_ignore_ascii_case("all");
    let reset_all = property.eq_ignore_ascii_case("all");

    // Resolve the property name and parse the value once, up front: a typo is
    // a usage error (exit 1), not a per-device failure.
    let request: Option<(&str, ParsedValue)> = if reset_all {
        None
    } else {
        let canonical = webcam::canonical_property_name(property)
            .with_context(|| format!("Unknown property '{property}'"))?;
        let value = match target {
            SetValue::Default => ParsedValue::Default,
            SetValue::Explicit(text) => webcam::parse_property_value(canonical, text)?,
        };
        Some((canonical, value))
    };

    let mut results: Vec<SetResult> = Vec::new();
    for idx in indices {
        let device = &devices[idx];
        let info = device.info();
        let device_name = info.display_name();

        // (canonical property name, value) pairs to write on this device.
        let jobs: Vec<(&str, ParsedValue)> = match request {
            None => info
                .properties()
                .map(|p| (p.name.as_str(), ParsedValue::Default))
                .collect(),
            Some((canonical, value)) => match info.property(canonical) {
                Some(p) => vec![(p.name.as_str(), value)],
                // With `--camera all`, devices that lack the property are
                // skipped so one virtual camera cannot fail a fleet-wide set.
                None if select_all => {
                    info!(
                        device_index = idx,
                        device_name,
                        property = canonical,
                        "Property not supported; skipped"
                    );
                    if output == OutputFormat::Text {
                        writeln!(
                            out,
                            "[{idx}] {device_name}: {canonical} not supported (skipped)"
                        )?;
                    }
                    Vec::new()
                }
                None => bail!("Property '{canonical}' not found on device '{device_name}'"),
            },
        };

        let entries = apply_jobs(device, idx, &jobs, restart_device)?;

        if output == OutputFormat::Text {
            for r in &entries {
                match (&r.error, &r.note) {
                    (Some(error), _) => writeln!(
                        out,
                        "[{idx}] {device_name}: Failed to set {} - {error}",
                        r.property
                    )?,
                    (None, Some(note)) => writeln!(
                        out,
                        "[{idx}] {device_name}: {} set to {} ({note})",
                        r.property, r.value
                    )?,
                    (None, None) => writeln!(
                        out,
                        "[{idx}] {device_name}: {} set to {}",
                        r.property, r.value
                    )?,
                }
            }
        }
        results.extend(entries);
    }

    if output == OutputFormat::Json {
        writeln!(out, "{}", render_json(&results)?)?;
    }

    Ok(if results.iter().all(|r| r.success) {
        Outcome::Success
    } else {
        Outcome::PartialFailure
    })
}

/// How long to wait for a restarted camera to re-enumerate before giving up.
const DEVICE_RESTART_TIMEOUT: Duration = Duration::from_secs(15);

/// Writes each job on one device, then reads every accepted write back through
/// a fresh handle. Some drivers keep a value only while an application has the
/// camera open; such a write is reported as a failure, not a success, unless
/// the driver stored it for the next device start. With `restart_device`, a
/// stored-but-pending write triggers a device restart so it takes effect now.
///
/// # Errors
/// Fails only when a requested device restart cannot be performed.
fn apply_jobs(
    device: &webcam::Device<'_>,
    idx: usize,
    jobs: &[(&str, ParsedValue)],
    restart_device: bool,
) -> Result<Vec<SetResult>> {
    let device_name = device.info().display_name();
    let mut entries: Vec<SetResult> = Vec::with_capacity(jobs.len());
    let mut accepted: Vec<(usize, &str, Written)> = Vec::new();

    for &(name, value) in jobs {
        let result = device.set(name, value);
        match &result {
            Ok(written) => {
                info!(
                    device_index = idx,
                    device_name,
                    property = name,
                    ?written,
                    "Property set"
                );
                accepted.push((entries.len(), name, *written));
            }
            Err(error) => {
                debug!(device_index = idx, device_name, property = name, ?value, %error, "Failed to set property");
            }
        }
        entries.push(SetResult {
            index: idx,
            name: device_name.to_owned(),
            property: name.to_owned(),
            value: match &result {
                Ok(written) => display_written(name, *written),
                Err(_) => display_requested(name, value),
            },
            success: result.is_ok(),
            note: None,
            error: result.err().map(|e| format!("{e:#}")),
        });
    }

    if accepted.is_empty() {
        return Ok(entries);
    }
    let names: Vec<&str> = accepted.iter().map(|(_, name, _)| *name).collect();
    match device.read_back(&names) {
        Ok(readings) => {
            for ((entry, name, written), reading) in accepted.iter().zip(readings) {
                if let Some(current) = reading
                    && !written.persisted_in(current)
                {
                    let now = display_current(name, current);
                    let e = &mut entries[*entry];
                    // The UVC class driver stores some controls and applies
                    // them at the next device start even when the camera drops
                    // them on close; that is a success with a caveat.
                    if device.stored_value(name) == Some(written.value) {
                        debug!(
                            device_index = idx,
                            device_name,
                            property = name,
                            ?written,
                            ?current,
                            "Write stored by the driver; pending device restart"
                        );
                        e.note = Some(format!(
                            "stored by the driver and applied when the camera next starts (reconnect it or reboot); \
                             until then the device reports {now} except while an application has it open"
                        ));
                    } else {
                        debug!(
                            device_index = idx,
                            device_name,
                            property = name,
                            ?written,
                            ?current,
                            "Write did not persist"
                        );
                        e.success = false;
                        e.error = Some(format!(
                            "the driver accepted the write but the device now reports {now}; \
                             this camera may keep the setting only while an application has it open"
                        ));
                    }
                }
            }
        }
        Err(error) => {
            debug!(device_index = idx, device_name, %error, "Could not read values back");
        }
    }

    if restart_device {
        restart_and_recheck(device, idx, &accepted, &mut entries)?;
    }
    Ok(entries)
}

/// Restarts the device if any write was only stored, then re-reads those writes.
///
/// # Errors
/// Fails when the restart cannot be performed or the device does not return.
fn restart_and_recheck(
    device: &webcam::Device<'_>,
    idx: usize,
    accepted: &[(usize, &str, Written)],
    entries: &mut [SetResult],
) -> Result<()> {
    let device_name = device.info().display_name();
    let pending: Vec<(usize, &str, Written)> = accepted
        .iter()
        .filter(|(entry, _, _)| entries[*entry].note.is_some())
        .copied()
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    info!(
        device_index = idx,
        device_name, "Restarting device to apply stored values"
    );
    device.restart()?;
    let names: Vec<&str> = pending.iter().map(|(_, name, _)| *name).collect();
    let readings = device.read_back_when_ready(&names, DEVICE_RESTART_TIMEOUT)?;
    for ((entry, name, written), reading) in pending.iter().zip(readings) {
        let e = &mut entries[*entry];
        match reading {
            Some(current) if written.persisted_in(current) => {
                e.note = Some("applied after restarting the device".to_owned());
            }
            Some(current) => {
                e.success = false;
                e.note = None;
                e.error = Some(format!(
                    "the device was restarted but still reports {}",
                    display_current(name, current)
                ));
            }
            None => {
                e.note = Some("device restarted; the value could not be read back".to_owned());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolves `--camera` to device indices: `all`, or one 0-based index.
fn parse_camera_selection(camera: &str, device_count: usize) -> Result<Vec<usize>> {
    if camera.len() > MAX_CAMERA_SELECTOR_LEN {
        bail!(
            "Camera selection exceeds the maximum length of {MAX_CAMERA_SELECTOR_LEN} characters"
        );
    }
    if camera.eq_ignore_ascii_case("all") {
        return Ok((0..device_count).collect());
    }
    if camera.is_empty() || !camera.chars().all(|c| c.is_ascii_digit()) {
        bail!("Invalid camera index '{camera}': must be a number or 'all'");
    }
    let idx: usize = camera
        .parse()
        .with_context(|| format!("Invalid camera index '{camera}'"))?;
    if idx >= device_count {
        bail!("Camera index {idx} not found (only {device_count} devices available)");
    }
    Ok(vec![idx])
}

/// What was actually sent to the driver, e.g. `50Hz` or `4000 [Auto]`.
fn display_written(property: &str, written: Written) -> String {
    let value = webcam::format_property_value(property, written.value);
    match written.mode {
        Mode::Manual => value,
        Mode::Auto => format!("{value} [Auto]"),
    }
}

/// What the user asked for, used when the write itself failed.
fn display_requested(property: &str, value: ParsedValue) -> String {
    match value {
        ParsedValue::Auto => "Auto".to_owned(),
        ParsedValue::Default => "default".to_owned(),
        ParsedValue::Manual(v) => webcam::format_property_value(property, v),
    }
}

/// What the device reports now, e.g. `50Hz` or `3534 [Auto]`.
fn display_current(property: &str, current: webcam::CurrentValue) -> String {
    let value = webcam::format_property_value(property, current.value);
    if current.flags & Mode::Auto.flag() != 0 {
        format!("{value} [Auto]")
    } else {
        value
    }
}

/// Converts a device's property list into display-ready output.
fn build_device_output(idx: usize, device: &webcam::DeviceInfo) -> DeviceOutput<'_> {
    let properties = device
        .properties()
        .map(|prop| {
            (
                prop.name.clone(),
                PropertyOutput {
                    value: prop
                        .current
                        .map(|c| webcam::format_property_value(&prop.name, c.value)),
                    mode: prop
                        .current
                        .and_then(|c| webcam::current_mode(prop.caps, c.flags))
                        .map(|m| m.to_string()),
                    default: webcam::format_property_value(&prop.name, prop.default),
                    min: prop.min,
                    max: prop.max,
                    step: prop.step,
                    supported_values: webcam::build_enum_display(&prop.name, prop.min, prop.max),
                    modes_supported: webcam::format_capabilities(prop.caps),
                },
            )
        })
        .collect();

    DeviceOutput {
        index: idx,
        name: device.display_name(),
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
/// numeric range, mirroring the drop-down versus slider split in the standard
/// DirectShow property dialog. The step is shown only when it is not 1.
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
        parse_camera_selection("3", 3).unwrap_err();
        parse_camera_selection("-1", 3).unwrap_err();
        parse_camera_selection("a", 3).unwrap_err();
        parse_camera_selection("", 3).unwrap_err();
        parse_camera_selection(&"1".repeat(MAX_CAMERA_SELECTOR_LEN + 1), 3).unwrap_err();
    }

    #[test]
    fn set_accepts_negative_values() {
        let cli = Cli::try_parse_from([
            "wincamcfg",
            "set",
            "-c",
            "0",
            "-p",
            "Exposure",
            "--value",
            "-5",
        ])
        .unwrap();
        match cli.command {
            Commands::Set { value, default, .. } => {
                assert_eq!(value.as_deref(), Some("-5"));
                assert!(!default);
            }
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn set_requires_exactly_one_of_value_and_default() {
        let conflict = Cli::try_parse_from([
            "wincamcfg",
            "set",
            "-c",
            "0",
            "-p",
            "Brightness",
            "--value",
            "1",
            "--default",
        ])
        .unwrap_err();
        assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);

        let missing =
            Cli::try_parse_from(["wincamcfg", "set", "-c", "0", "-p", "Brightness"]).unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);

        let cli = Cli::try_parse_from(["wincamcfg", "set", "-c", "all", "-p", "all", "--default"])
            .unwrap();
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
        let cli = Cli::try_parse_from([
            "wincamcfg",
            "set",
            "-c",
            "0",
            "-p",
            "PowerlineFrequency",
            "--value",
            "50Hz",
            "--restart-device",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Set {
                restart_device: true,
                ..
            }
        ));
        let cli = Cli::try_parse_from([
            "wincamcfg",
            "set",
            "-c",
            "0",
            "-p",
            "Brightness",
            "--default",
        ])
        .unwrap();
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
        let err = Cli::try_parse_from(["wincamcfg", "--version"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        assert!(err.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn display_values_use_labels_and_modes() {
        let manual = |value| Written {
            value,
            mode: Mode::Manual,
        };
        assert_eq!(display_written("PowerlineFrequency", manual(1)), "50Hz");
        assert_eq!(display_written("Brightness", manual(128)), "128");
        assert_eq!(
            display_written(
                "WhiteBalance",
                Written {
                    value: 4000,
                    mode: Mode::Auto
                }
            ),
            "4000 [Auto]"
        );
        assert_eq!(display_requested("Focus", ParsedValue::Auto), "Auto");
        assert_eq!(display_requested("Focus", ParsedValue::Default), "default");
        assert_eq!(
            display_current(
                "Exposure",
                webcam::CurrentValue {
                    value: -6,
                    flags: Mode::Auto.flag()
                }
            ),
            "-6 [Auto]"
        );
        assert_eq!(
            display_current(
                "PowerlineFrequency",
                webcam::CurrentValue { value: 1, flags: 0 }
            ),
            "50Hz"
        );
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
        let other = anyhow::anyhow!("something else");
        assert!(!is_broken_pipe(&other));
    }
}
