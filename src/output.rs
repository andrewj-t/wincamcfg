//! Output rows for `get` and `set`, and their text and JSON rendering.
//!
//! Every value is formatted into a string before it reaches a row, so the
//! JSON and text outputs show the same labels (`50Hz`, `On`) as the input side.

use std::fmt::Write as _;
use std::io::Write;

use anyhow::{Context, Result};
use indexmap::IndexMap;

use crate::webcam::{self, CurrentValue, DeviceInfo, ParsedValue, Property, Written};

// ---------------------------------------------------------------------------
// Output structures
// ---------------------------------------------------------------------------

/// One device with its formatted properties, for `get`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct DeviceOutput<'a> {
    index: usize,
    name: &'a str,
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

/// What was actually sent to the driver, e.g. `50Hz` or `4000 [Auto]`.
pub(crate) fn display_written(property: Property, written: Written) -> String {
    let value = webcam::format_property_value(property, written.value);
    match written.mode {
        webcam::Mode::Manual => value,
        webcam::Mode::Auto => format!("{value} [Auto]"),
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

/// What the device reports now, e.g. `50Hz` or `3534 [Auto]`.
pub(crate) fn display_current(property: Property, current: CurrentValue) -> String {
    let value = webcam::format_property_value(property, current.value);
    if current.is_auto() {
        format!("{value} [Auto]")
    } else {
        value
    }
}

/// Converts a device's property list into display-ready output.
pub(crate) fn build_device_output(idx: usize, device: &DeviceInfo) -> DeviceOutput<'_> {
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

pub(crate) fn render_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("Failed to serialize to JSON")
}

pub(crate) fn render_text(outputs: &[DeviceOutput], out: &mut dyn Write) -> Result<()> {
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
    use webcam::Mode;

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
