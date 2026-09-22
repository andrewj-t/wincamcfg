//! Property identifiers, modes, labels and value parsing: pure logic, no Windows calls.
//!
//! A [`Property`] carries the DirectShow interface it belongs to and its
//! numeric identifier. Values are plain `i32`s; a few properties are really
//! enumerations, and [`format_property_value`] / [`parse_property_value`]
//! translate between numbers and the labels users type (`50Hz`, `On`, ...).
//! A property can also run in `Auto` or `Manual` mode ([`Mode`]); the flag bits
//! are identical for both interfaces, which a compile-time assertion guarantees.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use tracing::trace;
use windows::Win32::Media::DirectShow::{
    CameraControl_Flags_Auto, CameraControl_Flags_Manual, VideoProcAmp_Flags_Auto,
    VideoProcAmp_Flags_Manual,
};

/// Which DirectShow interface a property belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropertyType {
    VideoProcAmp,
    CameraControl,
}

/// Every property this tool knows, across both DirectShow interfaces.
///
/// Identifiers come from `VideoProcAmpProperty` and `CameraControlProperty`
/// in `strmif.h`; they overlap, so [`Property::kind`] says which interface to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Property {
    Brightness,
    Contrast,
    Hue,
    Saturation,
    Sharpness,
    Gamma,
    ColorEnable,
    WhiteBalance,
    BacklightCompensation,
    Gain,
    DigitalMultiplier,
    DigitalMultiplierLimit,
    WhiteBalanceComponent,
    PowerlineFrequency,
    Pan,
    Tilt,
    Roll,
    Zoom,
    Exposure,
    Iris,
    Focus,
}

impl Property {
    /// Every property in query and display order: the standard dialog's tab
    /// order, then the properties the dialog does not show. Part of the output, keep stable.
    pub(crate) const ALL: [Self; 21] = [
        Self::Brightness,
        Self::Contrast,
        Self::Hue,
        Self::Saturation,
        Self::Sharpness,
        Self::Gamma,
        Self::WhiteBalance,
        Self::BacklightCompensation,
        Self::Gain,
        Self::ColorEnable,
        Self::PowerlineFrequency,
        Self::WhiteBalanceComponent,
        Self::DigitalMultiplier,
        Self::DigitalMultiplierLimit,
        Self::Zoom,
        Self::Focus,
        Self::Exposure,
        Self::Iris,
        Self::Pan,
        Self::Tilt,
        Self::Roll,
    ];

    /// Interface, numeric identifier and canonical name of the property.
    const fn spec(self) -> (PropertyType, i32, &'static str) {
        use PropertyType::{CameraControl, VideoProcAmp};
        match self {
            Self::Brightness => (VideoProcAmp, 0, "Brightness"),
            Self::Contrast => (VideoProcAmp, 1, "Contrast"),
            Self::Hue => (VideoProcAmp, 2, "Hue"),
            Self::Saturation => (VideoProcAmp, 3, "Saturation"),
            Self::Sharpness => (VideoProcAmp, 4, "Sharpness"),
            Self::Gamma => (VideoProcAmp, 5, "Gamma"),
            Self::ColorEnable => (VideoProcAmp, 6, "ColorEnable"),
            Self::WhiteBalance => (VideoProcAmp, 7, "WhiteBalance"),
            Self::BacklightCompensation => (VideoProcAmp, 8, "BacklightCompensation"),
            Self::Gain => (VideoProcAmp, 9, "Gain"),
            Self::DigitalMultiplier => (VideoProcAmp, 10, "DigitalMultiplier"),
            Self::DigitalMultiplierLimit => (VideoProcAmp, 11, "DigitalMultiplierLimit"),
            Self::WhiteBalanceComponent => (VideoProcAmp, 12, "WhiteBalanceComponent"),
            Self::PowerlineFrequency => (VideoProcAmp, 13, "PowerlineFrequency"),
            Self::Pan => (CameraControl, 0, "Pan"),
            Self::Tilt => (CameraControl, 1, "Tilt"),
            Self::Roll => (CameraControl, 2, "Roll"),
            Self::Zoom => (CameraControl, 3, "Zoom"),
            Self::Exposure => (CameraControl, 4, "Exposure"),
            Self::Iris => (CameraControl, 5, "Iris"),
            Self::Focus => (CameraControl, 6, "Focus"),
        }
    }

    pub(crate) const fn kind(self) -> PropertyType {
        self.spec().0
    }

    /// The identifier passed to the interface's `GetRange`, `Get` and `Set`.
    pub(crate) const fn id(self) -> i32 {
        self.spec().1
    }

    /// Canonical name, as shown in output and accepted (case-insensitively) on input.
    pub(crate) const fn as_str(self) -> &'static str {
        self.spec().2
    }
}

impl fmt::Display for Property {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Property {
    type Err = anyhow::Error;

    /// Resolves a user-typed name, ignoring ASCII case.
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str().eq_ignore_ascii_case(s))
            .with_context(|| format!("Unknown property '{s}'"))
    }
}

// ---------------------------------------------------------------------------
// Modes, labels, parsing
// ---------------------------------------------------------------------------

/// Whether the driver controls a property (`Auto`) or the user does (`Manual`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Auto,
    Manual,
}

// `Mode::flag` uses the VideoProcAmp constants for both interfaces; fail the
// build if the CameraControl ones ever diverge.
const _: () = assert!(
    VideoProcAmp_Flags_Auto.0 == CameraControl_Flags_Auto.0
        && VideoProcAmp_Flags_Manual.0 == CameraControl_Flags_Manual.0,
    "VideoProcAmp and CameraControl flag values diverged"
);

impl Mode {
    /// The DirectShow flag bit for this mode, valid for both interfaces.
    pub(crate) const fn flag(self) -> i32 {
        match self {
            Self::Auto => VideoProcAmp_Flags_Auto.0,
            Self::Manual => VideoProcAmp_Flags_Manual.0,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Manual => "Manual",
        }
    }

    const fn is_supported(self, caps: i32) -> bool {
        caps & self.flag() != 0
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The live value of a property together with its mode flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CurrentValue {
    pub value: i32,
    pub flags: i32,
}

impl CurrentValue {
    pub(crate) const fn is_auto(self) -> bool {
        self.flags & Mode::Auto.flag() != 0
    }
}

/// The current mode of a property, or `None` if it cannot switch modes (always manual).
#[must_use]
pub(crate) fn current_mode(caps: i32, current: CurrentValue) -> Option<Mode> {
    if !Mode::Auto.is_supported(caps) {
        return None;
    }
    Some(if current.is_auto() {
        Mode::Auto
    } else {
        Mode::Manual
    })
}

/// Formats capability flags as `"Manual"`, `"Auto"` or `"Manual, Auto"`.
#[must_use]
pub(crate) fn format_capabilities(caps: i32) -> Option<String> {
    let names: Vec<&str> = [Mode::Manual, Mode::Auto]
        .into_iter()
        .filter(|mode| mode.is_supported(caps))
        .map(Mode::as_str)
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

/// Value/label table for enumeration-like properties; values come from `ksmedia.h`.
fn value_labels(property: Property) -> Option<&'static [(i32, &'static str)]> {
    match property {
        Property::PowerlineFrequency => {
            Some(&[(0, "Disabled"), (1, "50Hz"), (2, "60Hz"), (3, "Auto")])
        }
        Property::ColorEnable | Property::BacklightCompensation => Some(&[(0, "Off"), (1, "On")]),
        _ => None,
    }
}

/// Formats a property value as its label if it has one, else as a number.
#[must_use]
pub(crate) fn format_property_value(property: Property, value: i32) -> String {
    match value_labels(property) {
        Some(labels) => labels.iter().find(|&&(v, _)| v == value).map_or_else(
            || format!("Unknown({value})"),
            |&(_, label)| label.to_owned(),
        ),
        None => value.to_string(),
    }
}

/// Lists the labels a property accepts within `[min, max]`, e.g. `"50Hz (1), 60Hz (2)"`.
#[must_use]
pub(crate) fn build_enum_display(property: Property, min: i32, max: i32) -> Option<String> {
    let labels = value_labels(property)?;
    let display = labels
        .iter()
        .filter(|&&(v, _)| v >= min && v <= max)
        .map(|&(val, label)| format!("{label} ({val})"))
        .collect::<Vec<_>>()
        .join(", ");
    (!display.is_empty()).then_some(display)
}

/// A value requested by the user for a property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParsedValue {
    /// Hand control to the driver.
    Auto,
    /// Set an explicit value and switch the property to manual mode.
    Manual(i32),
    /// Restore the driver's default, in Auto mode where supported (the dialog's Default button).
    Default,
}

/// Parses a user-supplied value such as `50Hz`, `On`, `Auto` or `-5`.
///
/// Labels win over the `Auto` keyword, so `Auto` on powerline frequency is the
/// label's value (3), not a mode switch. Anything else must be a decimal number.
pub(crate) fn parse_property_value(property: Property, value_str: &str) -> Result<ParsedValue> {
    let labels = value_labels(property);
    if let Some(labels) = labels
        && let Some(&(v, _)) = labels
            .iter()
            .find(|&&(_, label)| label.eq_ignore_ascii_case(value_str))
    {
        return Ok(ParsedValue::Manual(v));
    }
    if value_str.eq_ignore_ascii_case("auto") {
        return Ok(ParsedValue::Auto);
    }
    let parsed = value_str.parse::<i32>().with_context(|| match labels {
        Some(labels) => {
            let valid = labels
                .iter()
                .map(|&(_, l)| l)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Invalid value '{value_str}' for {property}. Expected one of: {valid}, or a number"
            )
        }
        None => format!("Invalid numeric value '{value_str}'"),
    })?;
    Ok(ParsedValue::Manual(parsed))
}

// ---------------------------------------------------------------------------
// Property data
// ---------------------------------------------------------------------------

/// Everything known about one supported property of a device.
#[derive(Debug, Clone)]
pub(crate) struct PropertyInfo {
    pub property: Property,
    pub min: i32,
    pub max: i32,
    pub step: i32,
    pub default: i32,
    pub caps: i32,
    /// `None` when the driver reported a range but refused to read the value.
    pub current: Option<CurrentValue>,
}

/// What a successful write sent to the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Written {
    pub value: i32,
    pub mode: Mode,
}

impl Written {
    /// Whether a read-back shows this write took effect: same value for Manual,
    /// the Auto flag for Auto (the driver then chooses the value).
    pub(crate) const fn persisted_in(self, current: CurrentValue) -> bool {
        match self.mode {
            Mode::Manual => current.value == self.value,
            Mode::Auto => current.is_auto(),
        }
    }
}

/// Turns a requested value into the value and mode sent to the driver.
///
/// `Auto` keeps the current (or default) value for drivers that insist on an
/// in-range value; `Default` re-enables Auto where supported, like the dialog's
/// Default button. A property reporting no capabilities is written in manual mode.
pub(crate) fn resolve_set(info: &PropertyInfo, value: ParsedValue) -> Result<Written> {
    let name = info.property;
    let auto = Mode::Auto.is_supported(info.caps);
    let (value, mode) = match value {
        ParsedValue::Auto if !auto => bail!(
            "Property '{name}' does not support Auto mode (supported modes: {})",
            format_capabilities(info.caps).unwrap_or_else(|| "none".to_owned())
        ),
        ParsedValue::Auto => (info.current.map_or(info.default, |c| c.value), Mode::Auto),
        ParsedValue::Default => (info.default, if auto { Mode::Auto } else { Mode::Manual }),
        ParsedValue::Manual(_) if info.caps != 0 && !Mode::Manual.is_supported(info.caps) => {
            bail!("Property '{name}' does not support Manual mode (supported modes: Auto)")
        }
        ParsedValue::Manual(v) if v < info.min || v > info.max => bail!(
            "Value {v} for property '{name}' is outside the supported range [{}, {}]",
            info.min,
            info.max
        ),
        ParsedValue::Manual(v) => {
            if info.step > 1 && (v - info.min) % info.step != 0 {
                trace!(property = %name, value = v, step = info.step, "Value is not on the step grid; the driver may round it");
            }
            (v, Mode::Manual)
        }
    };
    Ok(Written { value, mode })
}

// ---------------------------------------------------------------------------
// Tests (no hardware or COM required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const ENUM_PROPERTIES: [Property; 3] = [
        Property::PowerlineFrequency,
        Property::ColorEnable,
        Property::BacklightCompensation,
    ];

    fn info(
        property: Property,
        min: i32,
        max: i32,
        step: i32,
        default: i32,
        caps: i32,
    ) -> PropertyInfo {
        PropertyInfo {
            property,
            min,
            max,
            step,
            default,
            caps,
            current: None,
        }
    }

    fn written(value: i32, mode: Mode) -> Written {
        Written { value, mode }
    }

    #[test]
    fn labels_round_trip_through_format_and_parse() {
        for property in ENUM_PROPERTIES {
            let labels = value_labels(property).expect("enum property has labels");
            for &(value, label) in labels {
                assert_eq!(format_property_value(property, value), label);
                assert_eq!(
                    parse_property_value(property, label).unwrap(),
                    ParsedValue::Manual(value),
                    "{property}/{label}"
                );
                assert_eq!(
                    parse_property_value(property, &label.to_ascii_lowercase()).unwrap(),
                    ParsedValue::Manual(value),
                    "{property}/{label} lowercase"
                );
            }
        }
    }

    #[test]
    fn auto_label_wins_over_auto_mode_for_powerline_frequency() {
        for text in ["Auto", "AUTO"] {
            assert_eq!(
                parse_property_value(Property::PowerlineFrequency, text).unwrap(),
                ParsedValue::Manual(3)
            );
        }
    }

    #[test]
    fn auto_keyword_requests_auto_mode_elsewhere() {
        for (property, text) in [
            (Property::Brightness, "auto"),
            (Property::Exposure, "Auto"),
            (Property::ColorEnable, "auto"),
        ] {
            assert_eq!(
                parse_property_value(property, text).unwrap(),
                ParsedValue::Auto
            );
        }
    }

    #[test]
    fn numeric_values_parse_including_negatives() {
        for (property, text, value) in [
            (Property::Exposure, "-5", -5),
            (Property::Brightness, "128", 128),
            (Property::PowerlineFrequency, "1", 1),
        ] {
            assert_eq!(
                parse_property_value(property, text).unwrap(),
                ParsedValue::Manual(value)
            );
        }
    }

    #[test]
    fn invalid_values_are_rejected() {
        // The last one is an Arabic-Indic digit: a Unicode digit, but not a decimal number.
        for text in ["abc", "1 2", "", "99999999999", "\u{663}", "50Hz"] {
            parse_property_value(Property::Brightness, text).unwrap_err();
        }
        let err = parse_property_value(Property::PowerlineFrequency, "70Hz").unwrap_err();
        assert!(
            format!("{err:#}").contains("50Hz"),
            "error lists valid labels: {err:#}"
        );
    }

    #[test]
    fn unknown_enum_values_are_formatted_explicitly() {
        assert_eq!(
            format_property_value(Property::PowerlineFrequency, 7),
            "Unknown(7)"
        );
        assert_eq!(format_property_value(Property::Brightness, 7), "7");
    }

    #[test]
    fn enum_display_is_clipped_to_the_device_range() {
        assert_eq!(
            build_enum_display(Property::PowerlineFrequency, 1, 2).as_deref(),
            Some("50Hz (1), 60Hz (2)")
        );
        assert_eq!(build_enum_display(Property::PowerlineFrequency, 5, 9), None);
        assert_eq!(build_enum_display(Property::Brightness, 0, 255), None);
    }

    #[test]
    fn mode_reporting_follows_capabilities() {
        let auto = Mode::Auto.flag();
        let manual = Mode::Manual.flag();
        for caps in 0..=3 {
            for flags in [auto, manual] {
                let mode = current_mode(caps, CurrentValue { value: 0, flags });
                let expected = if caps & auto == 0 {
                    None
                } else if flags & auto != 0 {
                    Some(Mode::Auto)
                } else {
                    Some(Mode::Manual)
                };
                assert_eq!(mode, expected, "caps={caps} flags={flags}");
            }
        }
        assert_eq!(format_capabilities(0), None);
        assert_eq!(format_capabilities(manual).as_deref(), Some("Manual"));
        assert_eq!(format_capabilities(auto).as_deref(), Some("Auto"));
        assert_eq!(
            format_capabilities(auto | manual).as_deref(),
            Some("Manual, Auto")
        );
    }

    #[test]
    fn property_names_parse_case_insensitively() {
        assert_eq!(
            "brightness".parse::<Property>().unwrap(),
            Property::Brightness
        );
        assert_eq!("FOCUS".parse::<Property>().unwrap(), Property::Focus);
        assert_eq!(
            "powerlinefrequency".parse::<Property>().unwrap().as_str(),
            "PowerlineFrequency"
        );
        let err = "bogus".parse::<Property>().unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn property_table_is_consistent() {
        for p in Property::ALL {
            assert_eq!(p.as_str().parse::<Property>().unwrap(), p);
        }
        // Names identify a property on their own, so none may repeat.
        let names: HashSet<&str> = Property::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(names.len(), Property::ALL.len());
        // Within one interface every identifier is distinct.
        for kind in [PropertyType::VideoProcAmp, PropertyType::CameraControl] {
            let of_kind = || Property::ALL.iter().filter(|p| p.kind() == kind);
            let ids: HashSet<i32> = of_kind().map(|p| p.id()).collect();
            assert_eq!(ids.len(), of_kind().count(), "{kind:?}");
        }
    }

    #[test]
    fn resolve_set_validates_range_and_modes() {
        let both = Mode::Auto.flag() | Mode::Manual.flag();
        let p = info(Property::Exposure, -11, -1, 1, -6, both);
        assert_eq!(
            resolve_set(&p, ParsedValue::Manual(-5)).unwrap(),
            written(-5, Mode::Manual)
        );
        resolve_set(&p, ParsedValue::Manual(0)).unwrap_err();
        resolve_set(&p, ParsedValue::Manual(-12)).unwrap_err();
        // Auto keeps the default when no current value is known, else the current value.
        assert_eq!(
            resolve_set(&p, ParsedValue::Auto).unwrap(),
            written(-6, Mode::Auto)
        );
        let mut live = p.clone();
        live.current = Some(CurrentValue {
            value: -3,
            flags: Mode::Manual.flag(),
        });
        assert_eq!(
            resolve_set(&live, ParsedValue::Auto).unwrap(),
            written(-3, Mode::Auto)
        );

        let manual_only = info(Property::Brightness, 0, 255, 1, 128, Mode::Manual.flag());
        let err = resolve_set(&manual_only, ParsedValue::Auto).unwrap_err();
        assert!(err.to_string().contains("Manual"), "{err}");
        let auto_only = info(Property::Focus, 0, 255, 5, 0, Mode::Auto.flag());
        resolve_set(&auto_only, ParsedValue::Manual(10)).unwrap_err();
        // No capabilities reported at all: keep writing manual values as before.
        let no_caps = info(Property::Gamma, 100, 300, 1, 200, 0);
        assert_eq!(
            resolve_set(&no_caps, ParsedValue::Manual(150)).unwrap(),
            written(150, Mode::Manual)
        );
    }

    #[test]
    fn default_restores_the_default_value_in_auto_where_supported() {
        let both = Mode::Auto.flag() | Mode::Manual.flag();
        for (p, expected) in [
            (
                info(Property::WhiteBalance, 2000, 6500, 1, 4000, both),
                written(4000, Mode::Auto),
            ),
            (
                info(Property::Brightness, 0, 255, 1, 128, Mode::Manual.flag()),
                written(128, Mode::Manual),
            ),
            (
                info(Property::PowerlineFrequency, 1, 2, 1, 2, 0),
                written(2, Mode::Manual),
            ),
        ] {
            assert_eq!(resolve_set(&p, ParsedValue::Default).unwrap(), expected);
        }
    }

    #[test]
    fn persistence_check_compares_value_for_manual_and_flag_for_auto() {
        let current = |value, flags| CurrentValue { value, flags };
        let manual = written(2, Mode::Manual);
        assert!(manual.persisted_in(current(2, 0)));
        assert!(!manual.persisted_in(current(1, 0)));
        let auto = written(4000, Mode::Auto);
        assert!(auto.persisted_in(current(3534, Mode::Auto.flag())));
        assert!(!auto.persisted_in(current(4000, Mode::Manual.flag())));
    }
}
