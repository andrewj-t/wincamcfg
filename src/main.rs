pub mod webcam;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing::{debug, info, instrument};
use tracing_subscriber::EnvFilter;

// Output structures for JSON/text rendering
#[derive(Debug, serde::Serialize)]
struct DeviceOutput<'a> {
    index: usize,
    name: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    properties: Vec<(String, PropertyOutput)>,
}

// Property output with formatted values (value, default, and supported_values are all formatted strings)
#[derive(Debug, serde::Serialize)]
struct PropertyOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    supported_values: Option<String>,
}

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
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
        #[arg(long, default_value_t = false)]
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

        /// Property to set (e.g., PowerlineFrequency, Brightness, Contrast), or "all" to reset all properties (requires --default)
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

    debug!("Command: {:?}", std::env::args().collect::<Vec<_>>());

    match cli.command {
        Commands::List {
            include_device_path,
            output,
        } => list_devices(include_device_path, output)?,
        Commands::Get { camera, output } => get_device_properties(camera, output)?,
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

            // Validate property name early (skip validation for "all")
            if !property.eq_ignore_ascii_case("all")
                && (property.len() > 64 || !property.chars().all(|c| c.is_alphanumeric()))
            {
                anyhow::bail!("Invalid property name format");
            }

            // Validate value if provided
            if let Some(ref v) = value
                && v.len() > 32
            {
                anyhow::bail!("Property value exceeds maximum allowed length");
            }

            set_property(camera, property, value, default, output)?;
        }
    }

    Ok(())
}

/// Parse camera selection and return device indices
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
// Converts property vectors to output format with formatted values
fn build_device_output<'a>(idx: usize, device: &'a webcam::DeviceInfo) -> DeviceOutput<'a> {
    // Collect all properties from both VideoProcAmp and CameraControl
    let all_properties: Vec<&webcam::PropertyInfo> = device
        .video_proc_amp_properties
        .iter()
        .chain(&device.camera_control_properties)
        .collect();

    let property_outputs: Vec<(String, PropertyOutput)> = all_properties
        .iter()
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

// Serialize device outputs to pretty-printed JSON
fn render_json(outputs: &[DeviceOutput]) -> Result<String> {
    serde_json::to_string_pretty(outputs).context("Failed to serialize to JSON")
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

    // Add metadata if present (only in detailed mode)
    if prop.supported_values.is_some() || prop.default.is_some() {
        print!(" (");
        let mut first = true;

        if let Some(ref supported) = prop.supported_values {
            if !first {
                print!(", ");
            }
            print!("Supported: {}", supported);
            first = false;
        }
        if let Some(ref default) = prop.default {
            if !first {
                print!(", ");
            }
            print!("Default: {}", default);
        }
        print!(")");
    }
}

#[instrument(skip(output))]
fn list_devices(include_device_path: bool, output: OutputFormat) -> Result<()> {
    debug!(include_device_path, output_format = ?output, "Listing devices");

    // Get simple device list (index and name only)
    let devices = webcam::list_devices()?;

    info!("Found {} device(s)", devices.len());

    // Output in requested format
    match output {
        OutputFormat::Json => {
            let json_str = serde_json::to_string_pretty(&devices)?;
            println!("{}", json_str);
        }
        OutputFormat::Text => {
            if devices.is_empty() {
                println!("No video capture devices found.");
            } else {
                for device in devices {
                    if include_device_path {
                        if let Some(ref path) = device.device_path {
                            println!("[{}] {} ({})", device.index, device.name, path);
                        } else {
                            println!("[{}] {}", device.index, device.name);
                        }
                    } else {
                        println!("[{}] {}", device.index, device.name);
                    }
                }
            }
        }
    }

    Ok(())
}

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

    let mut results = Vec::new();

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
                        .unwrap_or_else(|| "".to_string());
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
                            .unwrap_or_else(|| "".to_string())
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Property '{}' not found on device '{}'",
                            property,
                            device_name
                        )
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
                Ok(_) => {
                    info!(device_index = idx, device_name, property = %prop_name, value = %prop_value, "Property set successfully")
                }
                Err(e) => {
                    debug!(device_index = idx, device_name, property = %prop_name, error = %e, "Failed to set property")
                }
            }

            results.push((
                idx,
                device_name,
                prop_name.to_string(),
                prop_value,
                result.is_ok(),
                result.err().map(|e| e.to_string()),
            ));
        }
    }

    // Output results
    match output {
        OutputFormat::Text => {
            for (idx, name, prop, value_set, success, error) in &results {
                if *success {
                    println!("[{}] {}: {} set to {}", idx, name, prop, value_set);
                } else {
                    println!(
                        "[{}] {}: Failed to set {} - {}",
                        idx,
                        name,
                        prop,
                        error.as_ref().unwrap_or(&"Unknown error".to_string())
                    );
                }
            }
        }
        OutputFormat::Json => {
            #[derive(serde::Serialize)]
            struct SetResult<'a> {
                index: usize,
                name: &'a str,
                property: &'a str,
                value: &'a str,
                success: bool,
                error: Option<&'a str>,
            }

            let json_results: Vec<SetResult> = results
                .iter()
                .map(|(idx, name, prop, value_set, success, error)| SetResult {
                    index: *idx,
                    name,
                    property: prop,
                    value: value_set,
                    success: *success,
                    error: error.as_deref(),
                })
                .collect();

            let json_str = serde_json::to_string_pretty(&json_results)?;
            println!("{}", json_str);
        }
    }

    Ok(())
}
