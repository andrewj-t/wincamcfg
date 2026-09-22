//! DirectShow webcam enumeration and property control.
//!
//! This module is the only place that talks to Windows. It wraps the DirectShow
//! COM interfaces used by the classic "camera properties" dialog:
//!
//! - `ICreateDevEnum` / `IEnumMoniker` to enumerate video capture devices,
//! - `IPropertyBag` to read a device's friendly name and device path,
//! - `IAMVideoProcAmp` (brightness, powerline frequency, ...) and
//!   `IAMCameraControl` (exposure, focus, ...) to read and write properties.
//!
//! # COM lifetime
//!
//! Every function that touches COM takes a [`ComSession`], which is the proof
//! that `CoInitializeEx` succeeded on the current thread. The session is
//! single-threaded (`!Send`), and [`Device`] handles borrow it, so the compiler
//! rejects any attempt to release a COM interface after `CoUninitialize` ran.
//! Command handlers should create the session first and let it drop last.
//!
//! COM is initialised as a single-threaded apartment. All activations use
//! `CLSCTX_INPROC_SERVER`, nothing is marshalled across apartments and no
//! callbacks are registered, so the STA never needs a message pump.
//!
//! # Values and modes
//!
//! Property values are plain `i32`s in DirectShow. A few properties are really
//! enumerations (powerline frequency, colour enable, backlight compensation);
//! [`format_property_value`] and [`parse_property_value`] translate between the
//! numbers and the labels users type (`50Hz`, `On`, ...). Independently of the
//! value, a property can run in `Auto` or `Manual` mode ([`Mode`]); the flag
//! bits are identical for both interfaces, which a compile-time assertion
//! guarantees.
//!
//! # Side effects
//!
//! Property writes go straight to the driver and persist across processes
//! exactly like changes made through the Windows camera dialog.

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tracing::{debug, instrument, trace};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::DirectShow::{
    CameraControl_Flags_Auto, CameraControl_Flags_Manual, IAMCameraControl, IAMVideoProcAmp,
    IBaseFilter, ICreateDevEnum, VideoProcAmp_Flags_Auto, VideoProcAmp_Flags_Manual,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoUninitialize, IEnumMoniker, IMoniker,
};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VariantClear};
use windows::core::{GUID, HSTRING, Interface};

// ---------------------------------------------------------------------------
// COM session
// ---------------------------------------------------------------------------

/// Proof that COM is initialised on the current thread.
///
/// Only [`ComSession::new`] can construct one, and the raw-pointer marker makes
/// it `!Send + !Sync`, so `CoUninitialize` always runs on the thread that called
/// `CoInitializeEx`. Everything that needs COM borrows a session, which ties the
/// lifetime of every COM interface to it.
#[derive(Debug)]
pub(crate) struct ComSession(PhantomData<*const ()>);

impl ComSession {
    /// Initialises COM as a single-threaded apartment for this thread.
    ///
    /// `S_FALSE` (already initialised by a host) counts as success; the matching
    /// `CoUninitialize` in `Drop` keeps the reference count balanced either way.
    ///
    /// # Errors
    /// Fails if `CoInitializeEx` reports an error, for example
    /// `RPC_E_CHANGED_MODE` when the thread already joined a multi-threaded
    /// apartment.
    pub(crate) fn new() -> Result<Self> {
        debug!("Initializing COM");
        // SAFETY: plain FFI call with no pointer arguments (`None` reserved
        // pointer). It is sound to call more than once on a thread; every
        // successful call is paired with `CoUninitialize` in `Drop`.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr.is_err() {
            bail!(
                "Failed to initialize COM ({hr:?}); RPC_E_CHANGED_MODE means the thread already \
                 uses a different apartment model"
            );
        }
        Ok(Self(PhantomData))
    }
}

impl Drop for ComSession {
    fn drop(&mut self) {
        // SAFETY: paired with the successful `CoInitializeEx` in `new()`. The
        // type is `!Send`, so this runs on the same thread, and every COM
        // interface borrowing this session has already been released because
        // the borrow checker forces them to drop first.
        unsafe { CoUninitialize() };
    }
}

// ---------------------------------------------------------------------------
// DirectShow identifiers
// ---------------------------------------------------------------------------

/// `CLSID_SystemDeviceEnum` from `uuids.h`: the system device enumerator.
const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);

/// `CLSID_VideoInputDeviceCategory` from `uuids.h`: the capture device category.
const CLSID_VIDEO_INPUT_DEVICE_CATEGORY: GUID =
    GUID::from_u128(0x860bb310_5d01_11d0_bd3b_00a0c911ce86);

/// `IAMVideoProcAmp` property identifiers (`VideoProcAmpProperty` in `strmif.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub(crate) enum VideoProcAmpProperty {
    Brightness = 0,
    Contrast = 1,
    Hue = 2,
    Saturation = 3,
    Sharpness = 4,
    Gamma = 5,
    ColorEnable = 6,
    WhiteBalance = 7,
    BacklightCompensation = 8,
    Gain = 9,
    DigitalMultiplier = 10,
    DigitalMultiplierLimit = 11,
    WhiteBalanceComponent = 12,
    PowerlineFrequency = 13,
}

impl VideoProcAmpProperty {
    /// Every variant, in the order properties are queried and displayed.
    ///
    /// Matches the "Video Proc Amp" tab of the standard DirectShow property
    /// dialog, followed by the properties that dialog does not show. This
    /// order is part of the user-visible output, so keep it stable.
    pub(crate) const ALL: [Self; 14] = [
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
    ];

    /// Canonical name, as shown in output and accepted (case-insensitively) on input.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Brightness => "Brightness",
            Self::Contrast => "Contrast",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Sharpness => "Sharpness",
            Self::Gamma => "Gamma",
            Self::ColorEnable => "ColorEnable",
            Self::WhiteBalance => "WhiteBalance",
            Self::BacklightCompensation => "BacklightCompensation",
            Self::Gain => "Gain",
            Self::DigitalMultiplier => "DigitalMultiplier",
            Self::DigitalMultiplierLimit => "DigitalMultiplierLimit",
            Self::WhiteBalanceComponent => "WhiteBalanceComponent",
            Self::PowerlineFrequency => "PowerlineFrequency",
        }
    }
}

impl fmt::Display for VideoProcAmpProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for VideoProcAmpProperty {
    type Err = UnknownPropertyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| UnknownPropertyError::new(s))
    }
}

impl From<VideoProcAmpProperty> for i32 {
    fn from(property: VideoProcAmpProperty) -> Self {
        property as Self
    }
}

/// `IAMCameraControl` property identifiers (`CameraControlProperty` in `strmif.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub(crate) enum CameraControlProperty {
    Pan = 0,
    Tilt = 1,
    Roll = 2,
    Zoom = 3,
    Exposure = 4,
    Iris = 5,
    Focus = 6,
}

impl CameraControlProperty {
    /// Every variant, in the order properties are queried and displayed.
    ///
    /// Matches the "Camera Control" tab of the standard DirectShow property
    /// dialog. This order is part of the user-visible output, so keep it stable.
    pub(crate) const ALL: [Self; 7] = [
        Self::Zoom,
        Self::Focus,
        Self::Exposure,
        Self::Iris,
        Self::Pan,
        Self::Tilt,
        Self::Roll,
    ];

    /// Canonical name, as shown in output and accepted (case-insensitively) on input.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pan => "Pan",
            Self::Tilt => "Tilt",
            Self::Roll => "Roll",
            Self::Zoom => "Zoom",
            Self::Exposure => "Exposure",
            Self::Iris => "Iris",
            Self::Focus => "Focus",
        }
    }
}

impl fmt::Display for CameraControlProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CameraControlProperty {
    type Err = UnknownPropertyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| UnknownPropertyError::new(s))
    }
}

impl From<CameraControlProperty> for i32 {
    fn from(property: CameraControlProperty) -> Self {
        property as Self
    }
}

/// Error returned when a property name matches neither interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnknownPropertyError {
    name: String,
}

impl UnknownPropertyError {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
        }
    }
}

impl fmt::Display for UnknownPropertyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown property '{}'", self.name)
    }
}

impl std::error::Error for UnknownPropertyError {}

/// Which DirectShow interface a property belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum PropertyType {
    VideoProcAmp,
    CameraControl,
}

impl fmt::Display for PropertyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::VideoProcAmp => "VideoProcAmp",
            Self::CameraControl => "CameraControl",
        })
    }
}

/// Resolves a user-typed property name to its canonical spelling.
///
/// Returns `None` when the name matches neither interface.
#[must_use]
pub(crate) fn canonical_property_name(name: &str) -> Option<&'static str> {
    name.parse::<VideoProcAmpProperty>()
        .map(VideoProcAmpProperty::as_str)
        .or_else(|_| {
            name.parse::<CameraControlProperty>()
                .map(CameraControlProperty::as_str)
        })
        .ok()
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

// Both interfaces define Auto = 1 and Manual = 2. `Mode::flag` relies on that
// identity to use one set of constants for both; fail the build if it changes.
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

/// Returns the current mode of a property, or `None` if it cannot switch modes.
///
/// A property that does not advertise `Auto` in its capabilities is always
/// manual, so reporting a mode for it would only add noise.
#[must_use]
pub(crate) fn current_mode(caps: i32, flags: i32) -> Option<Mode> {
    if !Mode::Auto.is_supported(caps) {
        return None;
    }
    Some(if flags & Mode::Auto.flag() != 0 {
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

/// Value/label table for enumeration-like properties.
///
/// Values come from `ksmedia.h` (`KSPROPERTY_VIDEOPROCAMP_POWERLINE_FREQUENCY`
/// uses 0 = disabled, 1 = 50 Hz, 2 = 60 Hz, 3 = auto; boolean properties use
/// 0 = off, 1 = on).
fn value_labels(property_name: &str) -> Option<&'static [(i32, &'static str)]> {
    match property_name {
        "PowerlineFrequency" => Some(&[(0, "Disabled"), (1, "50Hz"), (2, "60Hz"), (3, "Auto")]),
        "ColorEnable" | "BacklightCompensation" => Some(&[(0, "Off"), (1, "On")]),
        _ => None,
    }
}

/// Formats a property value as its label if it has one, else as a number.
#[must_use]
pub(crate) fn format_property_value(property_name: &str, value: i32) -> String {
    match value_labels(property_name) {
        Some(labels) => labels.iter().find(|&&(v, _)| v == value).map_or_else(
            || format!("Unknown({value})"),
            |&(_, label)| label.to_owned(),
        ),
        None => value.to_string(),
    }
}

/// Lists the labels a property accepts within `[min, max]`, e.g. `"50Hz (1), 60Hz (2)"`.
#[must_use]
pub(crate) fn build_enum_display(property_name: &str, min: i32, max: i32) -> Option<String> {
    let labels = value_labels(property_name)?;
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
}

/// Longest accepted `--value` string.
///
/// The longest label is `Disabled` (8) and the longest `i32` is 11 characters;
/// anything longer is not a value this tool understands.
const MAX_VALUE_LEN: usize = 32;

/// Parses a user-supplied value such as `50Hz`, `On`, `Auto` or `-5`.
///
/// Labels are checked before the `Auto` keyword, so a property whose label
/// table contains `Auto` (powerline frequency) gets that value rather than a
/// mode switch. Everything else that is not a label must be a decimal number.
///
/// # Errors
/// Fails when the string is longer than [`MAX_VALUE_LEN`], contains characters
/// other than ASCII letters, digits, `-` and space, or is neither a known label
/// nor a number.
pub(crate) fn parse_property_value(property_name: &str, value_str: &str) -> Result<ParsedValue> {
    if value_str.len() > MAX_VALUE_LEN {
        bail!("Value exceeds the maximum length of {MAX_VALUE_LEN} characters");
    }
    if !value_str
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ' ')
    {
        bail!("Value contains invalid characters (only ASCII letters, digits, '-' and space)");
    }

    let labels = value_labels(property_name);

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
            let valid = labels.iter().map(|&(_, l)| l).collect::<Vec<_>>().join(", ");
            format!("Invalid value '{value_str}' for {property_name}. Expected one of: {valid}, or a number")
        }
        None => format!("Invalid numeric value '{value_str}'"),
    })?;
    Ok(ParsedValue::Manual(parsed))
}

// ---------------------------------------------------------------------------
// Device data
// ---------------------------------------------------------------------------

/// Range, default and capability flags reported by `GetRange`.
#[derive(Debug, Clone, Copy)]
struct PropertyRange {
    min: i32,
    max: i32,
    step: i32,
    default: i32,
    caps: i32,
}

/// The live value of a property together with its mode flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CurrentValue {
    pub value: i32,
    pub flags: i32,
}

/// Everything known about one supported property of a device.
#[derive(Debug, Clone)]
pub(crate) struct PropertyInfo {
    pub name: String,
    pub min: i32,
    pub max: i32,
    pub step: i32,
    pub default: i32,
    pub caps: i32,
    /// `None` when the driver reported a range but refused to read the value.
    pub current: Option<CurrentValue>,
    pub property_type: PropertyType,
}

/// Plain data describing a device and its supported properties.
///
/// Holds no COM interfaces, so it may outlive the [`ComSession`].
#[derive(Debug, Clone)]
pub(crate) struct DeviceInfo {
    pub name: Option<String>,
    pub video_proc_amp_properties: Vec<PropertyInfo>,
    pub camera_control_properties: Vec<PropertyInfo>,
}

impl DeviceInfo {
    /// All supported properties, `VideoProcAmp` first, in query order.
    pub(crate) fn properties(&self) -> impl Iterator<Item = &PropertyInfo> {
        self.video_proc_amp_properties
            .iter()
            .chain(&self.camera_control_properties)
    }

    /// Looks a property up by name, ignoring ASCII case.
    pub(crate) fn property(&self, name: &str) -> Option<&PropertyInfo> {
        self.properties()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// The friendly name, or `"Unknown"` when the driver did not provide one.
    pub(crate) fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("Unknown")
    }
}

/// Name and path of a device, for the `list` command.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DeviceListItem {
    pub index: usize,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_path: Option<String>,
}

/// A capture device bound for the lifetime of a [`ComSession`].
///
/// The borrowed session guarantees the moniker is released before COM shuts
/// down; use [`Device::info`] for the plain data.
#[derive(Debug)]
pub(crate) struct Device<'com> {
    moniker: IMoniker,
    info: DeviceInfo,
    _com: PhantomData<&'com ComSession>,
}

impl Device<'_> {
    pub(crate) fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Writes a property on this device.
    ///
    /// The name is matched case-insensitively against the properties the
    /// device reported; the value is validated against the reported range and
    /// capabilities before the driver is called.
    ///
    /// # Errors
    /// Fails when the device does not support the property, the value is out of
    /// range, the requested mode is not supported, or the driver rejects the
    /// write.
    #[instrument(skip(self), fields(device = %self.info.display_name()))]
    pub(crate) fn set(&self, property: &str, value: ParsedValue) -> Result<()> {
        let info = self.info.property(property).with_context(|| {
            format!(
                "Property '{property}' not found on device '{}'",
                self.info.display_name()
            )
        })?;
        let (raw_value, flags) = resolve_set(info, value)?;

        let filter = bind_filter(&self.moniker)?;
        // The stored name is canonical, so parsing it cannot fail for a
        // property the device reported; the error path only guards data bugs.
        let result = match info.property_type {
            PropertyType::VideoProcAmp => {
                let id: VideoProcAmpProperty = info.name.parse()?;
                filter
                    .cast::<IAMVideoProcAmp>()
                    .context("Failed to get IAMVideoProcAmp interface")?
                    .set(id.into(), raw_value, flags)
            }
            PropertyType::CameraControl => {
                let id: CameraControlProperty = info.name.parse()?;
                filter
                    .cast::<IAMCameraControl>()
                    .context("Failed to get IAMCameraControl interface")?
                    .set(id.into(), raw_value, flags)
            }
        };
        result.with_context(|| {
            format!("Failed to set {} to {raw_value} (flags {flags})", info.name)
        })?;
        debug!(property = %info.name, value = raw_value, flags, "Property set");
        Ok(())
    }
}

/// Turns a requested value into the `(value, flags)` pair the driver expects.
///
/// `Auto` keeps the current value (or the default) so drivers that insist on an
/// in-range value even in auto mode are satisfied. A property that advertises
/// capabilities is only switched to a mode it supports; a property reporting no
/// capabilities at all is written in manual mode as before.
fn resolve_set(info: &PropertyInfo, value: ParsedValue) -> Result<(i32, i32)> {
    let name = &info.name;
    match value {
        ParsedValue::Auto => {
            if !Mode::Auto.is_supported(info.caps) {
                bail!(
                    "Property '{name}' does not support Auto mode (supported modes: {})",
                    format_capabilities(info.caps).unwrap_or_else(|| "none".to_owned())
                );
            }
            let keep = info.current.map_or(info.default, |c| c.value);
            Ok((keep, Mode::Auto.flag()))
        }
        ParsedValue::Manual(v) => {
            if info.caps != 0 && !Mode::Manual.is_supported(info.caps) {
                bail!("Property '{name}' does not support Manual mode (supported modes: Auto)");
            }
            if v < info.min || v > info.max {
                bail!(
                    "Value {v} for property '{name}' is outside the supported range [{}, {}]",
                    info.min,
                    info.max
                );
            }
            if info.step > 1 && (v - info.min) % info.step != 0 {
                trace!(property = %name, value = v, step = info.step, "Value is not on the step grid; the driver may round it");
            }
            Ok((v, Mode::Manual.flag()))
        }
    }
}

// ---------------------------------------------------------------------------
// Property interface abstraction
// ---------------------------------------------------------------------------

/// Common shape of `IAMVideoProcAmp` and `IAMCameraControl`.
///
/// Both interfaces expose the same `GetRange`/`Get`/`Set` triple over different
/// property identifiers; this trait lets one generic query routine serve both.
trait PropertyControl: Interface {
    type Property: Copy + Into<i32> + fmt::Display + 'static;
    const KIND: PropertyType;
    const QUERY_ORDER: &'static [Self::Property];

    fn range(&self, id: i32) -> windows::core::Result<PropertyRange>;
    fn get(&self, id: i32) -> windows::core::Result<CurrentValue>;
    fn set(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()>;
}

impl PropertyControl for IAMVideoProcAmp {
    type Property = VideoProcAmpProperty;
    const KIND: PropertyType = PropertyType::VideoProcAmp;
    const QUERY_ORDER: &'static [Self::Property] = &VideoProcAmpProperty::ALL;

    fn range(&self, id: i32) -> windows::core::Result<PropertyRange> {
        let mut r = PropertyRange {
            min: 0,
            max: 0,
            step: 0,
            default: 0,
            caps: 0,
        };
        // SAFETY: `self` is a live interface; every out-pointer refers to a
        // local `i32` that outlives the call.
        unsafe {
            self.GetRange(
                id,
                &raw mut r.min,
                &raw mut r.max,
                &raw mut r.step,
                &raw mut r.default,
                &raw mut r.caps,
            )
        }?;
        Ok(r)
    }

    fn get(&self, id: i32) -> windows::core::Result<CurrentValue> {
        let mut c = CurrentValue { value: 0, flags: 0 };
        // SAFETY: `self` is a live interface; both out-pointers refer to local
        // `i32`s that outlive the call.
        unsafe { self.Get(id, &raw mut c.value, &raw mut c.flags) }?;
        Ok(c)
    }

    fn set(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()> {
        // SAFETY: `self` is a live interface; the arguments are plain integers.
        unsafe { self.Set(id, value, flags) }
    }
}

impl PropertyControl for IAMCameraControl {
    type Property = CameraControlProperty;
    const KIND: PropertyType = PropertyType::CameraControl;
    const QUERY_ORDER: &'static [Self::Property] = &CameraControlProperty::ALL;

    fn range(&self, id: i32) -> windows::core::Result<PropertyRange> {
        let mut r = PropertyRange {
            min: 0,
            max: 0,
            step: 0,
            default: 0,
            caps: 0,
        };
        // SAFETY: `self` is a live interface; every out-pointer refers to a
        // local `i32` that outlives the call.
        unsafe {
            self.GetRange(
                id,
                &raw mut r.min,
                &raw mut r.max,
                &raw mut r.step,
                &raw mut r.default,
                &raw mut r.caps,
            )
        }?;
        Ok(r)
    }

    fn get(&self, id: i32) -> windows::core::Result<CurrentValue> {
        let mut c = CurrentValue { value: 0, flags: 0 };
        // SAFETY: `self` is a live interface; both out-pointers refer to local
        // `i32`s that outlive the call.
        unsafe { self.Get(id, &raw mut c.value, &raw mut c.flags) }?;
        Ok(c)
    }

    fn set(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()> {
        // SAFETY: `self` is a live interface; the arguments are plain integers.
        unsafe { self.Set(id, value, flags) }
    }
}

/// Queries every property of one interface that the filter supports.
///
/// A property whose `GetRange` fails is treated as unsupported and skipped; a
/// property whose `Get` fails is reported without a current value.
fn query_properties<C: PropertyControl>(filter: &IBaseFilter) -> Result<Vec<PropertyInfo>> {
    let iface: C = filter
        .cast()
        .with_context(|| format!("Device does not expose the {} interface", C::KIND))?;

    let mut properties = Vec::with_capacity(C::QUERY_ORDER.len());
    for &property in C::QUERY_ORDER {
        let id: i32 = property.into();
        let Ok(range) = iface.range(id) else {
            trace!(property = %property, "GetRange failed; property not supported");
            continue;
        };
        let current = iface.get(id).ok();
        trace!(property = %property, ?range, ?current, "Property queried");
        properties.push(PropertyInfo {
            name: property.to_string(),
            min: range.min,
            max: range.max,
            step: range.step,
            default: range.default,
            caps: range.caps,
            current,
            property_type: C::KIND,
        });
    }
    debug!(kind = %C::KIND, count = properties.len(), "Properties enumerated");
    Ok(properties)
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/// Lists capture devices by name and path without touching their filters.
///
/// Only the property bag is read, so a device whose driver stalls when bound
/// cannot stall a plain `list`.
///
/// # Errors
/// Fails if the system device enumerator cannot be created or the video input
/// category cannot be enumerated.
#[instrument(skip_all)]
pub(crate) fn list_devices(com: &ComSession) -> Result<Vec<DeviceListItem>> {
    let monikers = video_input_monikers(com)?;
    Ok(monikers
        .iter()
        .enumerate()
        .map(|(index, moniker)| DeviceListItem {
            index,
            name: read_bag_string(moniker, "FriendlyName").unwrap_or_else(|_| "Unknown".to_owned()),
            device_path: read_bag_string(moniker, "DevicePath").ok(),
        })
        .collect())
}

/// Enumerates capture devices and reads every supported property of each.
///
/// Each device is bound to its filter once; a device that cannot be bound is
/// still returned, with empty property lists, so indices stay stable between
/// `list` and `get`.
///
/// # Errors
/// Fails if the system device enumerator cannot be created or the video input
/// category cannot be enumerated.
#[instrument(skip_all)]
pub(crate) fn open_devices(com: &ComSession) -> Result<Vec<Device<'_>>> {
    let monikers = video_input_monikers(com)?;
    let mut devices = Vec::with_capacity(monikers.len());

    for moniker in monikers {
        let name = read_bag_string(&moniker, "FriendlyName").ok();
        debug!(?name, "Processing device");

        let (video_proc_amp_properties, camera_control_properties) = match bind_filter(&moniker) {
            Ok(filter) => (
                query_properties::<IAMVideoProcAmp>(&filter).unwrap_or_default(),
                query_properties::<IAMCameraControl>(&filter).unwrap_or_default(),
            ),
            Err(error) => {
                debug!(?name, %error, "Could not bind device filter; reporting no properties");
                (Vec::new(), Vec::new())
            }
        };

        devices.push(Device {
            moniker,
            info: DeviceInfo {
                name,
                video_proc_amp_properties,
                camera_control_properties,
            },
            _com: PhantomData,
        });
    }

    debug!(count = devices.len(), "Device enumeration complete");
    Ok(devices)
}

/// Collects the monikers of every device in the video input category.
fn video_input_monikers(_com: &ComSession) -> Result<Vec<IMoniker>> {
    // SAFETY: COM is initialised on this thread (a `ComSession` is borrowed);
    // the CLSID is a valid static and the returned interface type is checked
    // against its IID by windows-rs.
    let dev_enum: ICreateDevEnum =
        unsafe { CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER) }
            .context("Failed to create system device enumerator")?;

    let mut enum_moniker: Option<IEnumMoniker> = None;
    // SAFETY: `dev_enum` is a live interface; the category GUID is a valid
    // static and `enum_moniker` is a valid out-slot for the call's duration.
    unsafe {
        dev_enum.CreateClassEnumerator(&CLSID_VIDEO_INPUT_DEVICE_CATEGORY, &raw mut enum_moniker, 0)
    }
    .context("Failed to create video input device enumerator")?;

    // S_FALSE with a null enumerator means the category is empty.
    let Some(enum_moniker) = enum_moniker else {
        debug!("No video input devices found");
        return Ok(Vec::new());
    };

    let mut monikers = Vec::new();
    loop {
        let mut slot: [Option<IMoniker>; 1] = [None];
        let mut fetched = 0u32;
        // SAFETY: `enum_moniker` is a live interface; `slot` is a valid
        // one-element out-slice and `fetched` a valid out-pointer for the
        // call's duration. `Next` returns S_FALSE once the enumeration is
        // exhausted.
        let hr = unsafe { enum_moniker.Next(&mut slot, Some(&raw mut fetched)) };
        if hr != S_OK || fetched == 0 {
            trace!(?hr, fetched, "Enumeration complete");
            break;
        }
        if let Some(moniker) = slot[0].take() {
            monikers.push(moniker);
        }
    }
    debug!(count = monikers.len(), "Video input monikers collected");
    Ok(monikers)
}

/// Binds a moniker to the device's `IBaseFilter`, which activates the driver.
fn bind_filter(moniker: &IMoniker) -> Result<IBaseFilter> {
    // SAFETY: `moniker` is a live interface; no bind context or left moniker
    // is required, and the result type is checked against its IID.
    unsafe { moniker.BindToObject(None, None) }.context("Failed to bind to device filter")
}

/// A `VARIANT` that is always cleared, whatever happens after it is filled.
#[derive(Default)]
struct OwnedVariant(VARIANT);

impl Drop for OwnedVariant {
    fn drop(&mut self) {
        // SAFETY: `self.0` is an initialised VARIANT (VT_EMPTY from `Default`
        // or filled by `IPropertyBag::Read`); `VariantClear` releases whatever
        // it owns and resets it to VT_EMPTY. The result is irrelevant while
        // dropping.
        let _ = unsafe { VariantClear(&raw mut self.0) };
    }
}

/// Reads a string-valued entry (`FriendlyName`, `DevicePath`) from a device's property bag.
fn read_bag_string(moniker: &IMoniker, property: &str) -> Result<String> {
    // SAFETY: `moniker` is a live interface; the result type is checked
    // against its IID.
    let bag: IPropertyBag = unsafe { moniker.BindToStorage(None, None) }
        .with_context(|| format!("Failed to bind property bag for '{property}'"))?;

    let name = HSTRING::from(property);
    let mut var = OwnedVariant::default();
    // SAFETY: `bag` is a live interface; `name` is a valid NUL-terminated wide
    // string that outlives the call; `var.0` is a valid, initialised VARIANT
    // out-parameter; no error log is supplied.
    unsafe { bag.Read(&name, &raw mut var.0, None) }
        .with_context(|| format!("Failed to read property '{property}'"))?;

    // SAFETY: `vt` is always initialised and identifies the active union member.
    let vt = unsafe { var.0.Anonymous.Anonymous.vt };
    if vt != VT_BSTR {
        bail!("Property '{property}' is not a string (VARTYPE {})", vt.0);
    }
    // SAFETY: `vt == VT_BSTR` was checked, so `bstrVal` is the active member.
    // The BSTR is only borrowed (never moved out of its `ManuallyDrop`), and
    // `OwnedVariant::drop` frees it via `VariantClear`.
    let value = unsafe { &var.0.Anonymous.Anonymous.Anonymous.bstrVal }.to_string();
    trace!(property, value = %value, "Property bag entry read");
    Ok(value)
}

// ---------------------------------------------------------------------------
// Tests (no hardware or COM required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ENUM_PROPERTIES: [&str; 3] =
        ["PowerlineFrequency", "ColorEnable", "BacklightCompensation"];

    fn info(name: &str, min: i32, max: i32, step: i32, default: i32, caps: i32) -> PropertyInfo {
        PropertyInfo {
            name: name.to_owned(),
            min,
            max,
            step,
            default,
            caps,
            current: None,
            property_type: PropertyType::VideoProcAmp,
        }
    }

    #[test]
    fn labels_round_trip_through_format_and_parse() {
        for name in ENUM_PROPERTIES {
            let labels = value_labels(name).expect("enum property has labels");
            for &(value, label) in labels {
                assert_eq!(format_property_value(name, value), label);
                assert_eq!(
                    parse_property_value(name, label).unwrap(),
                    ParsedValue::Manual(value),
                    "{name}/{label}"
                );
                assert_eq!(
                    parse_property_value(name, &label.to_ascii_lowercase()).unwrap(),
                    ParsedValue::Manual(value),
                    "{name}/{label} lowercase"
                );
            }
        }
    }

    #[test]
    fn auto_label_wins_over_auto_mode_for_powerline_frequency() {
        assert_eq!(
            parse_property_value("PowerlineFrequency", "Auto").unwrap(),
            ParsedValue::Manual(3)
        );
        assert_eq!(
            parse_property_value("PowerlineFrequency", "AUTO").unwrap(),
            ParsedValue::Manual(3)
        );
    }

    #[test]
    fn auto_keyword_requests_auto_mode_elsewhere() {
        assert_eq!(
            parse_property_value("Brightness", "auto").unwrap(),
            ParsedValue::Auto
        );
        assert_eq!(
            parse_property_value("Exposure", "Auto").unwrap(),
            ParsedValue::Auto
        );
        assert_eq!(
            parse_property_value("ColorEnable", "auto").unwrap(),
            ParsedValue::Auto
        );
    }

    #[test]
    fn numeric_values_parse_including_negatives() {
        assert_eq!(
            parse_property_value("Exposure", "-5").unwrap(),
            ParsedValue::Manual(-5)
        );
        assert_eq!(
            parse_property_value("Brightness", "128").unwrap(),
            ParsedValue::Manual(128)
        );
        assert_eq!(
            parse_property_value("PowerlineFrequency", "1").unwrap(),
            ParsedValue::Manual(1)
        );
    }

    #[test]
    fn invalid_values_are_rejected() {
        parse_property_value("Brightness", "abc").unwrap_err();
        parse_property_value("Brightness", "1 2").unwrap_err();
        parse_property_value("Brightness", &"1".repeat(MAX_VALUE_LEN + 1)).unwrap_err();
        // Arabic-Indic digit three: Unicode-alphanumeric but not ASCII.
        parse_property_value("Brightness", "\u{663}").unwrap_err();
        parse_property_value("Brightness", "50Hz").unwrap_err();
        let err = parse_property_value("PowerlineFrequency", "70Hz").unwrap_err();
        assert!(
            format!("{err:#}").contains("50Hz"),
            "error lists valid labels: {err:#}"
        );
    }

    #[test]
    fn unknown_enum_values_are_formatted_explicitly() {
        assert_eq!(format_property_value("PowerlineFrequency", 7), "Unknown(7)");
        assert_eq!(format_property_value("Brightness", 7), "7");
    }

    #[test]
    fn enum_display_is_clipped_to_the_device_range() {
        assert_eq!(
            build_enum_display("PowerlineFrequency", 1, 2).as_deref(),
            Some("50Hz (1), 60Hz (2)")
        );
        assert_eq!(build_enum_display("PowerlineFrequency", 5, 9), None);
        assert_eq!(build_enum_display("Brightness", 0, 255), None);
    }

    #[test]
    fn mode_reporting_follows_capabilities() {
        let auto = Mode::Auto.flag();
        let manual = Mode::Manual.flag();
        for caps in 0..=3 {
            for flags in [auto, manual] {
                let mode = current_mode(caps, flags);
                if caps & auto == 0 {
                    assert_eq!(mode, None, "caps={caps} flags={flags}");
                } else if flags & auto != 0 {
                    assert_eq!(mode, Some(Mode::Auto), "caps={caps} flags={flags}");
                } else {
                    assert_eq!(mode, Some(Mode::Manual), "caps={caps} flags={flags}");
                }
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
            "brightness".parse::<VideoProcAmpProperty>().unwrap(),
            VideoProcAmpProperty::Brightness
        );
        assert_eq!(
            "FOCUS".parse::<CameraControlProperty>().unwrap(),
            CameraControlProperty::Focus
        );
        let err = "bogus".parse::<VideoProcAmpProperty>().unwrap_err();
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn canonical_names_resolve_case_insensitively() {
        assert_eq!(
            canonical_property_name("powerlinefrequency"),
            Some("PowerlineFrequency")
        );
        assert_eq!(canonical_property_name("FOCUS"), Some("Focus"));
        assert_eq!(canonical_property_name("bogus"), None);
    }

    #[test]
    fn every_property_round_trips_through_its_name() {
        for p in VideoProcAmpProperty::ALL {
            assert_eq!(p.as_str().parse::<VideoProcAmpProperty>().unwrap(), p);
        }
        for p in CameraControlProperty::ALL {
            assert_eq!(p.as_str().parse::<CameraControlProperty>().unwrap(), p);
        }
        // The two name spaces must not overlap, or a name could not identify its interface.
        for v in VideoProcAmpProperty::ALL {
            v.as_str().parse::<CameraControlProperty>().unwrap_err();
        }
    }

    #[test]
    fn resolve_set_validates_range_and_modes() {
        let both = Mode::Auto.flag() | Mode::Manual.flag();
        let p = info("Exposure", -11, -1, 1, -6, both);
        assert_eq!(
            resolve_set(&p, ParsedValue::Manual(-5)).unwrap(),
            (-5, Mode::Manual.flag())
        );
        resolve_set(&p, ParsedValue::Manual(0)).unwrap_err();
        resolve_set(&p, ParsedValue::Manual(-12)).unwrap_err();
        // Auto keeps the default when no current value is known.
        assert_eq!(
            resolve_set(&p, ParsedValue::Auto).unwrap(),
            (-6, Mode::Auto.flag())
        );
        // ...and the current value when it is.
        let mut live = p.clone();
        live.current = Some(CurrentValue {
            value: -3,
            flags: Mode::Manual.flag(),
        });
        assert_eq!(
            resolve_set(&live, ParsedValue::Auto).unwrap(),
            (-3, Mode::Auto.flag())
        );

        let manual_only = info("Brightness", 0, 255, 1, 128, Mode::Manual.flag());
        let err = resolve_set(&manual_only, ParsedValue::Auto).unwrap_err();
        assert!(err.to_string().contains("Manual"), "{err}");

        let auto_only = info("Focus", 0, 255, 5, 0, Mode::Auto.flag());
        resolve_set(&auto_only, ParsedValue::Manual(10)).unwrap_err();

        // No capabilities reported at all: keep writing manual values as before.
        let no_caps = info("Gamma", 100, 300, 1, 200, 0);
        assert_eq!(
            resolve_set(&no_caps, ParsedValue::Manual(150)).unwrap(),
            (150, Mode::Manual.flag())
        );
    }

    #[test]
    fn device_info_lookup_ignores_case() {
        let device = DeviceInfo {
            name: None,
            video_proc_amp_properties: vec![info("Brightness", 0, 255, 1, 128, 2)],
            camera_control_properties: vec![info("Focus", 0, 255, 1, 0, 3)],
        };
        assert_eq!(
            device.property("BRIGHTNESS").map(|p| p.name.as_str()),
            Some("Brightness")
        );
        assert_eq!(
            device.property("focus").map(|p| p.name.as_str()),
            Some("Focus")
        );
        assert!(device.property("Zoom").is_none());
        assert_eq!(device.display_name(), "Unknown");
        assert_eq!(device.properties().count(), 2);
    }
}
