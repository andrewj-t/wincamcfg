//! Output rows for `get` and `set`, and their text and JSON rendering.
//!
//! Every value is formatted into a string before it reaches a row, so the
//! JSON and text outputs show the same labels (`50Hz`, `On`) as the input side.

use std::fmt::Write as _;
use std::io::Write;

use anyhow::{Context, Result};
use indexmap::IndexMap;

use crate::webcam::{self, DeviceInfo, DriverInfo, ParsedValue, Property, PropertyInfo};

// ---------------------------------------------------------------------------
// Output structures
// ---------------------------------------------------------------------------

/// One device with its formatted properties, for `get`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct DeviceOutput {
    index: usize,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    driver: Option<DriverInfo>,
    properties: IndexMap<String, PropertyOutput>,
}

/// One property with every value already formatted for display.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PropertyOutput {
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
pub(crate) struct SetResult {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) property: String,
    pub(crate) value: String,
    pub(crate) success: bool,
    /// Set when the write succeeded with a caveat the caller should know about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// A value with its mode, e.g. `50Hz` or `4000 [Auto]`.
pub(crate) fn display_value(property: Property, value: i32, auto: bool) -> String {
    let value = webcam::format_property_value(property, value);
    if auto {
        format!("{value} [Auto]")
    } else {
        value
    }
}

/// What the user asked for, used when the write itself failed.
pub(crate) fn display_requested(property: Property, value: ParsedValue) -> String {
    match value {
        ParsedValue::Auto => "Auto".to_owned(),
        ParsedValue::Default => "default".to_owned(),
        ParsedValue::Manual(v) => webcam::format_property_value(property, v),
    }
}

impl From<&PropertyInfo> for PropertyOutput {
    fn from(prop: &PropertyInfo) -> Self {
        let property = prop.property;
        Self {
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
        }
    }
}

/// Converts a device's property list into display-ready output.
pub(crate) fn build_device_output(idx: usize, device: &DeviceInfo) -> DeviceOutput {
    DeviceOutput {
        index: idx,
        name: device.name.clone(),
        driver: device.driver.clone(),
        properties: device
            .properties
            .iter()
            .map(|prop| (prop.property.to_string(), PropertyOutput::from(prop)))
            .collect(),
    }
}

pub(crate) fn render_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("Failed to serialize to JSON")
}

pub(crate) fn render_text(outputs: &[DeviceOutput], out: &mut dyn Write) -> Result<()> {
    for output in outputs {
        writeln!(out, "[{}] {}", output.index, output.name)?;
        if let Some(driver) = &output.driver {
            writeln!(out, "  Driver:")?;
            for line in driver_lines(driver) {
                writeln!(out, "    {line}")?;
            }
        }
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

/// The lines of the `Driver:` block in `get` text output, one per field the registry had.
fn driver_lines(driver: &DriverInfo) -> Vec<String> {
    [
        ("Description", &driver.description),
        ("Manufacturer", &driver.manufacturer),
        ("Provider", &driver.provider),
        ("Version", &driver.version),
        ("Date", &driver.date),
        ("INF", &driver.inf_path),
    ]
    .into_iter()
    .filter_map(|(label, value)| value.as_ref().map(|v| format!("{label}: {v}")))
    .collect()
}

/// Writes one text line per `set` result row.
pub(crate) fn render_set_rows(rows: &[SetResult], out: &mut dyn Write) -> Result<()> {
    for r in rows {
        let line = match (&r.error, &r.note) {
            (Some(error), _) => format!("Failed to set {} - {error}", r.property),
            (None, Some(note)) => format!("{} set to {} ({note})", r.property, r.value),
            (None, None) => format!("{} set to {}", r.property, r.value),
        };
        writeln!(out, "[{}] {}: {line}", r.index, r.name)?;
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

    #[test]
    fn display_values_use_labels_and_modes() {
        assert_eq!(
            display_value(Property::PowerlineFrequency, 1, false),
            "50Hz"
        );
        assert_eq!(display_value(Property::Brightness, 128, false), "128");
        assert_eq!(
            display_value(Property::WhiteBalance, 4000, true),
            "4000 [Auto]"
        );
        assert_eq!(display_value(Property::Exposure, -6, true), "-6 [Auto]");
        assert_eq!(
            display_requested(Property::Focus, ParsedValue::Auto),
            "Auto"
        );
        assert_eq!(
            display_requested(Property::Focus, ParsedValue::Default),
            "default"
        );
        assert_eq!(
            display_requested(Property::Gain, ParsedValue::Manual(3)),
            "3"
        );
    }
    #[test]
    fn driver_block_lists_only_known_fields() {
        let driver = DriverInfo {
            manufacturer: Some("Logitech".to_owned()),
            version: Some("1.4.40.0".to_owned()),
            ..DriverInfo::default()
        };
        assert_eq!(
            driver_lines(&driver),
            ["Manufacturer: Logitech", "Version: 1.4.40.0"]
        );
        assert!(driver_lines(&DriverInfo::default()).is_empty());
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
}
