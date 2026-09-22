//! wincamcfg: command-line control of webcam properties on Windows.
//!
//! This file holds the clap definitions and the entry point. Subcommands live
//! in [`commands`], output formatting in [`output`], and all DirectShow work in
//! [`webcam`]. Diagnostics go to stderr through `tracing`, controlled by
//! `RUST_LOG` (default `warn`).

mod commands;
mod output;
mod webcam;

use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use tracing::debug;
use tracing_subscriber::filter::LevelFilter;

/// Exit code for usage errors and failed enumeration.
const EXIT_ERROR: u8 = 1;

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
pub(crate) enum OutputFormat {
    Text,
    Json,
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
        } => commands::list_devices(include_device_path, output, out)?,
        Commands::Get { camera, output } => commands::get_device_properties(&camera, output, out)?,
        Commands::Dialog { camera } => commands::open_dialog(&camera, out)?,
        Commands::Set {
            camera,
            property,
            value,
            restart_device,
            output,
            ..
        } => {
            return commands::set_property(
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
// Tests (no hardware or COM required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    fn parse(command_line: &str) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(command_line.split(' '))
    }

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
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
    fn broken_pipe_is_detected_through_context() {
        let err =
            anyhow::Error::from(io::Error::from(io::ErrorKind::BrokenPipe)).context("writing");
        assert!(is_broken_pipe(&err));
        assert!(!is_broken_pipe(&anyhow::anyhow!("something else")));
    }
}
