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

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use indexmap::IndexMap;
use tracing::{debug, info};
use tracing_subscriber::filter::LevelFilter;

use webcam::{ComSession, ParsedValue};

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

        /// Output format
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
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
        Commands::Version => writeln!(out, "wincamcfg {}", env!("CARGO_PKG_VERSION"))?,
        Commands::Set {
            camera,
            property,
            value,
            output,
            ..
        } => {
            // clap's `target` group guarantees exactly one of --value/--default.
            let target = value.map_or(SetValue::Default, SetValue::Explicit);
            if property.eq_ignore_ascii_case("all") && target != SetValue::Default {
                bail!("Property 'all' can only be used with --default");
            }
            return set_property(&camera, &property, &target, output, out);
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

fn set_property(
    camera: &str,
    property: &str,
    target: &SetValue,
    output: OutputFormat,
    out: &mut dyn Write,
) -> Result<Outcome> {
    debug!(camera, property, ?target, ?output, "Setting property");

    let com = ComSession::new()?;
    let devices = webcam::open_devices(&com).context("Failed to enumerate devices")?;
    let indices = parse_camera_selection(camera, devices.len())?;
    let select_all = camera.eq_ignore_ascii_case("all");
    let reset_all = property.eq_ignore_ascii_case("all");

    // Resolve the property name and parse the value once, up front: a typo is
    // a usage error (exit 1), not a per-device failure.
    let request: Option<(&str, Option<ParsedValue>)> = if reset_all {
        None
    } else {
        let canonical = webcam::canonical_property_name(property)
            .with_context(|| format!("Unknown property '{property}'"))?;
        let value = match target {
            SetValue::Default => None,
            SetValue::Explicit(text) => Some(webcam::parse_property_value(canonical, text)?),
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
                .map(|p| (p.name.as_str(), ParsedValue::Manual(p.default)))
                .collect(),
            Some((canonical, value)) => match info.property(canonical) {
                Some(p) => vec![(
                    p.name.as_str(),
                    value.unwrap_or(ParsedValue::Manual(p.default)),
                )],
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

        for (name, value) in jobs {
            let result = device.set(name, value);
            match &result {
                Ok(()) => info!(
                    device_index = idx,
                    device_name,
                    property = name,
                    ?value,
                    "Property set"
                ),
                Err(error) => {
                    debug!(device_index = idx, device_name, property = name, ?value, %error, "Failed to set property");
                }
            }
            let entry = SetResult {
                index: idx,
                name: device_name.to_owned(),
                property: name.to_owned(),
                value: display_value(name, value),
                success: result.is_ok(),
                error: result.err().map(|e| format!("{e:#}")),
            };
            // Text output streams as it happens so it interleaves correctly
            // with skip notices; JSON is emitted once at the end.
            if output == OutputFormat::Text {
                match &entry.error {
                    None => writeln!(out, "[{idx}] {device_name}: {name} set to {}", entry.value)?,
                    Some(error) => {
                        writeln!(out, "[{idx}] {device_name}: Failed to set {name} - {error}")?;
                    }
                }
            }
            results.push(entry);
        }
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

/// The value as it will be reported back to the user.
fn display_value(property: &str, value: ParsedValue) -> String {
    match value {
        ParsedValue::Auto => "Auto".to_owned(),
        ParsedValue::Manual(v) => webcam::format_property_value(property, v),
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

/// Formats one property as `value [mode] (Supported: ..., Modes: ..., Default: ...)`.
fn format_property_line(prop: &PropertyOutput) -> String {
    let Some(current) = &prop.value else {
        return "<unavailable>".to_owned();
    };

    let mut line = current.clone();
    if let Some(mode) = &prop.mode {
        // Writing to a String cannot fail.
        let _ = write!(line, " [{mode}]");
    }

    let mut meta = Vec::with_capacity(3);
    if let Some(supported) = &prop.supported_values {
        meta.push(format!("Supported: {supported}"));
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
    fn version_flag_is_available() {
        let err = Cli::try_parse_from(["wincamcfg", "--version"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        assert!(err.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn display_value_uses_labels() {
        assert_eq!(
            display_value("PowerlineFrequency", ParsedValue::Manual(1)),
            "50Hz"
        );
        assert_eq!(display_value("Brightness", ParsedValue::Manual(128)), "128");
        assert_eq!(display_value("Focus", ParsedValue::Auto), "Auto");
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
