//! CLI entry point for wincamcfg; parses arguments and dispatches to webcam operations.

pub mod webcam;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use indexmap::IndexMap;
use mimalloc::MiMalloc;
use serde_with::skip_serializing_none;
use tracing::{debug, info, instrument};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Output structures for JSON/text rendering
#[skip_serializing_none]
#[derive(Debug, serde::Serialize)]
struct DeviceOutput<'a> {
    index: usize,
    name: &'a str,
    properties: IndexMap<String, PropertyOutput>,
}

// Property output with formatted values (value, default, and supported_values are all formatted strings)
#[skip_serializing_none]
#[derive(Debug, serde::Serialize)]
struct PropertyOutput {
    value: Option<String>,
    default: Option<String>,
    supported_values: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, serde::Serialize)]
struct SetResult {
    index: usize,
    name: String,
    property: String,
    value: String,
    success: bool,
    error: Option<String>,
}

/// A command-line utility for managing webcam properties
#[derive(Parser)]
#[command(name = "wincamcfg")]
#[command(about = "Manage webcam properties")]
#[command(
    long_about = "A command line utility for managing webcam configuration on windows.\n\nConfigure camera properties like brightness, contrast, focus, exposure, and more using DirectShow APIs."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
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
    Set {
        /// Camera index from list command (0-based), or "all" for all cameras
        #[arg(short, long)]
        camera: String,

        /// Property to set (e.g., `PowerlineFrequency`, `Brightness`, `Contrast`), or "all" to reset all properties (requires --default)
        #[arg(short, long)]
        property: String,

        /// Value to set
        #[arg(short, long, conflicts_with = "default")]
        value: Option<String>,

        /// Set to default value
        #[arg(short, long, conflicts_with = "value")]
        default: bool,

        /// Output format
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },

    /// Show version information
    Version,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing with environment-based filtering
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .init();

    debug!(args = ?std::env::args().collect::<Vec<String>>(), "cli.invoked");

    match cli.command {
        Commands::List {
            include_device_path,
            output,
        } => list_devices(include_device_path, output)?,
        Commands::Get { camera, output } => get_device_properties(camera, output)?,
        Commands::Version => print_version(),
        Commands::Set {
            camera,
            property,
            value,
            default,
            output,
        } => {
            if !default && value.is_none() {
                anyhow::bail!("Either --value or --default must be specified");
            }

            // Check if property is "all" - only allowed with --default
            if property.eq_ignore_ascii_case("all") && !default {
                anyhow::bail!("Property 'all' can only be used with --default flag");
            }

            set_property(camera, property, value, default, output)?;
        }
    }

    Ok(())
}

/// Parse camera selection and return device indices.
///
/// # Errors
///
/// Returns an error if the input exceeds 16 characters, contains non-digit characters
/// (when not "all"), or if the index is out of bounds.
fn parse_camera_selection(camera: &str, device_count: usize) -> Result<Vec<usize>> {
    // Sanitize input: limit length
    if camera.len() > 16 {
        anyhow::bail!("Camera selection string exceeds maximum allowed length");
    }

    if camera.eq_ignore_ascii_case("all") {
        Ok((0..device_count).collect())
    } else {
        // Only allow digits for camera index
        if !camera.chars().all(|c| c.is_ascii_digit()) {
            anyhow::bail!("Invalid camera index: must be a number or 'all'");
        }

        let idx: usize = camera
            .parse()
            .with_context(|| format!("Invalid camera index: {}", camera))?;

        if idx >= device_count {
            anyhow::bail!(
                "Camera index {} not found (only {} devices available)",
                idx,
                device_count
            );
        }

        Ok(vec![idx])
    }
}

// Build device output structure from domain DeviceInfo
// Converts property vectors to IndexMap with formatted values
fn build_device_output(idx: usize, device: &webcam::DeviceInfo) -> DeviceOutput<'_> {
    // Collect all properties from both VideoProcAmp and CameraControl
    let property_outputs: IndexMap<String, PropertyOutput> = device
        .video_proc_amp_properties
        .iter()
        .chain(&device.camera_control_properties)
        .map(|prop| {
            (
                prop.name.clone(),
                PropertyOutput {
                    value: prop
                        .current
                        .map(|v| webcam::format_property_value(&prop.name, v)),
                    default: prop
                        .default
                        .map(|v| webcam::format_property_value(&prop.name, v)),
                    supported_values: prop
                        .min
                        .and_then(|min| prop.max.map(|max| (min, max)))
                        .and_then(|(min, max)| webcam::build_enum_display(&prop.name, min, max)),
                },
            )
        })
        .collect();

    DeviceOutput {
        index: idx,
        name: device.name.as_deref().unwrap_or("Unknown"),
        properties: property_outputs,
    }
}

// Serialize any serializable value to pretty-printed JSON
fn render_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("Failed to serialize to JSON")
}

// Render device outputs as human-readable text
// Shows properties with formatted values
fn render_text(outputs: &[DeviceOutput]) {
    for output in outputs {
        println!("[{}] {}", output.index, output.name);
        println!("  Properties:");

        if output.properties.is_empty() {
            println!("    No properties available");
        } else {
            for (name, prop) in &output.properties {
                print!("    {}: ", name);
                display_property_value(prop);
                println!();
            }
        }

        println!();
    }
}

// Display a single property value with metadata (Supported values and Default)
fn display_property_value(prop: &PropertyOutput) {
    let Some(ref current) = prop.value else {
        print!("<unavailable>");
        return;
    };

    // Display the value (already formatted)
    print!("{}", current);

    // Add metadata if present
    let mut meta = Vec::new();
    if let Some(ref supported) = prop.supported_values {
        meta.push(format!("Supported: {supported}"));
    }
    if let Some(ref default) = prop.default {
        meta.push(format!("Default: {default}"));
    }
    if !meta.is_empty() {
        print!(" ({})", meta.join(", "));
    }
}

/// List all video capture devices.
///
/// # Errors
///
/// Returns an error if device enumeration fails or if JSON serialization fails.
#[instrument(skip(output))]
fn list_devices(include_device_path: bool, output: OutputFormat) -> Result<()> {
    debug!(include_device_path, output_format = ?output, "Listing devices");

    // Get simple device list (index and name only)
    let devices = webcam::list_devices()?;

    info!("Found {} device(s)", devices.len());

    // Output in requested format
    match output {
        OutputFormat::Json => {
            println!("{}", render_json(&devices)?);
        }
        OutputFormat::Text => {
            if devices.is_empty() {
                println!("No video capture devices found.");
            } else {
                for device in devices {
                    let suffix = if include_device_path {
                        device
                            .device_path
                            .as_deref()
                            .map(|p| format!(" ({p})"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    println!("[{}] {}{suffix}", device.index, device.name);
                }
            }
        }
    }

    Ok(())
}

/// Get property values from specified device(s).
///
/// # Errors
///
/// Returns an error if device enumeration fails, camera index is invalid, or JSON serialization fails.
#[expect(
    clippy::needless_pass_by_value,
    reason = "clap passes String and OutputFormat by value"
)]
#[instrument(skip(output))]
fn get_device_properties(camera: String, output: OutputFormat) -> Result<()> {
    debug!(camera = %camera, output_format = ?output, "Getting device properties");

    let devices = webcam::enumerate_devices().context("Failed to enumerate devices")?;

    let indices = parse_camera_selection(&camera, devices.len())?;

    let outputs: Vec<DeviceOutput> = indices
        .iter()
        .map(|&idx| build_device_output(idx, &devices[idx]))
        .collect();

    match output {
        OutputFormat::Text => render_text(&outputs),
        OutputFormat::Json => println!("{}", render_json(&outputs)?),
    }

    Ok(())
}

/// Set a property value on specified device(s).
///
/// # Errors
///
/// Returns an error if device enumeration fails, camera index is invalid, property is not found,
/// value is out of range, or the set operation fails.
#[expect(
    clippy::needless_pass_by_value,
    reason = "clap passes String and OutputFormat by value"
)]
#[instrument(skip(output))]
fn set_property(
    camera: String,
    property: String,
    value: Option<String>,
    use_default: bool,
    output: OutputFormat,
) -> Result<()> {
    debug!(camera = %camera, property = %property, value = ?value, use_default, output_format = ?output, "Setting property");

    let devices = webcam::enumerate_devices().context("Failed to enumerate devices")?;

    let indices = parse_camera_selection(&camera, devices.len())?;

    // Check if we're resetting all properties
    let reset_all = property.eq_ignore_ascii_case("all");

    let mut results: Vec<SetResult> = Vec::new();

    for &idx in &indices {
        let device = &devices[idx];
        let device_name = device.name.as_deref().unwrap_or("Unknown");

        // Get list of properties to set
        let properties_to_set: Vec<(&str, String)> = if reset_all {
            // Collect all properties with their default values, formatted as strings
            device
                .video_proc_amp_properties
                .iter()
                .chain(&device.camera_control_properties)
                .map(|p| {
                    let val = p
                        .default
                        .map(|v| webcam::format_property_value(&p.name, v))
                        .unwrap_or_default();
                    (p.name.as_str(), val)
                })
                .collect()
        } else {
            // Single property
            let value_to_set = if use_default {
                // Find the property to get its default value, formatted as string
                device
                    .video_proc_amp_properties
                    .iter()
                    .chain(&device.camera_control_properties)
                    .find(|p| p.name.eq_ignore_ascii_case(&property))
                    .map(|p| {
                        p.default
                            .map(|v| webcam::format_property_value(&p.name, v))
                            .unwrap_or_default()
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("Property '{property}' not found on device '{device_name}'")
                    })?
            } else {
                value.clone().unwrap() // Safe because we validated earlier
            };
            vec![(property.as_str(), value_to_set)]
        };

        // Set each property
        for (prop_name, prop_value) in properties_to_set {
            let result = webcam::set_property(device, prop_name, &prop_value);

            match &result {
                Ok(()) => {
                    info!(device_index = idx, device_name, property = %prop_name, value = %prop_value, "Property set successfully");
                }
                Err(e) => {
                    debug!(device_index = idx, device_name, property = %prop_name, error = %e, "Failed to set property");
                }
            }

            results.push(SetResult {
                index: idx,
                name: device_name.to_string(),
                property: prop_name.to_string(),
                value: prop_value,
                success: result.is_ok(),
                error: result.err().map(|e| e.to_string()),
            });
        }
    }

    // Output results
    match output {
        OutputFormat::Text => {
            for r in &results {
                if r.success {
                    println!(
                        "[{}] {}: {} set to {}",
                        r.index, r.name, r.property, r.value
                    );
                } else {
                    println!(
                        "[{}] {}: Failed to set {} - {}",
                        r.index,
                        r.name,
                        r.property,
                        r.error.as_deref().unwrap_or("Unknown error")
                    );
                }
            }
        }
        OutputFormat::Json => {
            println!("{}", render_json(&results)?);
        }
    }

    Ok(())
}

fn print_version() {
    println!("wincamcfg {}", env!("CARGO_PKG_VERSION"));
}
