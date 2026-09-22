//! One handler per subcommand: enumerate, select cameras, act, and write rows.

use std::io::Write;

use anyhow::{Context, Result, bail};
use tracing::{debug, info};

use crate::OutputFormat;
use crate::output::{
    DeviceOutput, SetResult, build_device_output, display_requested, display_value, render_json,
    render_set_rows, render_text,
};
use crate::webcam::{
    self, ComSession, CurrentValue, Mode, ParsedValue, Persistence, Property, PropertyInfo,
    WriteOutcome,
};

pub(crate) fn list_devices(
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

pub(crate) fn get_device_properties(
    camera: &str,
    output: OutputFormat,
    out: &mut dyn Write,
) -> Result<()> {
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

pub(crate) fn open_dialog(camera: &str, out: &mut dyn Write) -> Result<()> {
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

/// What `set` writes on each selected device.
#[derive(Debug, Clone, Copy)]
enum Request {
    /// `--property all --default`: every supported property back to its default.
    ResetAll,
    One(Property, ParsedValue),
}

/// Resolves `--property` and `--value` once, up front, so a typo is a usage
/// error (exit 1) rather than a per-device failure. `value` is `None` for
/// `--default`; clap guarantees exactly one of the two was given.
fn parse_request(property: &str, value: Option<&str>) -> Result<Request> {
    if property.eq_ignore_ascii_case("all") {
        if value.is_some() {
            bail!("Property 'all' can only be used with --default");
        }
        return Ok(Request::ResetAll);
    }
    let property: Property = property.parse()?;
    let value = match value {
        None => ParsedValue::Default,
        Some(text) => webcam::parse_property_value(property, text)?,
    };
    Ok(Request::One(property, value))
}

/// Runs `set`; returns whether every write succeeded.
pub(crate) fn set_property(
    camera: &str,
    property: &str,
    value: Option<&str>,
    restart_device: bool,
    output: OutputFormat,
    out: &mut dyn Write,
) -> Result<bool> {
    debug!(
        camera,
        property,
        ?value,
        restart_device,
        ?output,
        "Setting property"
    );
    let request = parse_request(property, value)?;
    if restart_device && !webcam::is_elevated() {
        bail!("--restart-device needs administrator rights; run this from an elevated prompt");
    }
    let com = ComSession::new()?;
    let devices = webcam::open_devices(&com).context("Failed to enumerate devices")?;
    let select_all = camera.eq_ignore_ascii_case("all");

    let mut results: Vec<SetResult> = Vec::new();
    for idx in parse_camera_selection(camera, devices.len())? {
        let device = &devices[idx];
        let device_name = device.info.name.as_str();
        let jobs: Vec<(&PropertyInfo, ParsedValue)> = match request {
            Request::ResetAll => device
                .info
                .properties
                .iter()
                .map(|p| (p, ParsedValue::Default))
                .collect(),
            Request::One(property, value) => match device.info.property(property) {
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
                    continue;
                }
                None => bail!("Property '{property}' not found on device '{device_name}'"),
            },
        };
        let rows: Vec<SetResult> = device
            .write_all(&jobs, restart_device)?
            .into_iter()
            .zip(&jobs)
            .map(|(outcome, &(prop, value))| {
                set_result(idx, device_name, prop.property, value, outcome)
            })
            .collect();
        if output == OutputFormat::Text {
            render_set_rows(&rows, out)?;
        }
        results.extend(rows);
    }

    if output == OutputFormat::Json {
        writeln!(out, "{}", render_json(&results)?)?;
    }
    Ok(results.iter().all(|r| r.success))
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
            row.value = display_value(
                property,
                report.written.value,
                report.written.mode == Mode::Auto,
            );
            let now =
                |current: CurrentValue| display_value(property, current.value, current.is_auto());
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

// ---------------------------------------------------------------------------
// Tests (no hardware or COM required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use webcam::{WriteReport, Written};

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
}
