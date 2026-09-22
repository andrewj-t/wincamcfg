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
//! Every property this tool knows is a [`Property`], which carries the
//! interface it belongs to and its numeric identifier. Property values are
//! plain `i32`s in DirectShow. A few properties are really enumerations
//! (powerline frequency, colour enable, backlight compensation);
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
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tracing::{debug, info, instrument, trace};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Disable_DevNode, CM_Enable_DevNode, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CONFIGRET,
    CR_ACCESS_DENIED, CR_SUCCESS,
};
use windows::Win32::Foundation::{HWND, S_OK};
use windows::Win32::Media::DirectShow::{
    CameraControl_Flags_Auto, CameraControl_Flags_Manual, IAMCameraControl, IAMVideoProcAmp,
    IBaseFilter, ICreateDevEnum, VideoProcAmp_Flags_Auto, VideoProcAmp_Flags_Manual,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize, IEnumMoniker, IMoniker,
};
use windows::Win32::System::Ole::{ISpecifyPropertyPages, OleCreatePropertyFrame};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::IsUserAnAdmin;
use windows::core::{BSTR, GUID, HSTRING, IUnknown, Interface};

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

/// Which DirectShow interface a property belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropertyType {
    VideoProcAmp,
    CameraControl,
}

/// Every property this tool knows, across both DirectShow interfaces.
///
/// The numeric identifiers come from `VideoProcAmpProperty` and
/// `CameraControlProperty` in `strmif.h`. They overlap between the two
/// interfaces, so [`Property::kind`] says which interface to call.
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
    /// Every property, in the order they are queried and displayed.
    ///
    /// Matches the "Video Proc Amp" and "Camera Control" tabs of the standard
    /// DirectShow property dialog, each followed by the properties that tab
    /// does not show. This order is part of the user-visible output, so keep
    /// it stable.
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

    /// The DirectShow interface that exposes this property.
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

/// The live value of a property together with its mode flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CurrentValue {
    pub value: i32,
    pub flags: i32,
}

impl CurrentValue {
    /// Whether the driver reports the property as running in Auto mode.
    pub(crate) const fn is_auto(self) -> bool {
        self.flags & Mode::Auto.flag() != 0
    }
}

/// Returns the current mode of a property, or `None` if it cannot switch modes.
///
/// A property that does not advertise `Auto` in its capabilities is always
/// manual, so reporting a mode for it would only add noise.
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

/// Value/label table for enumeration-like properties.
///
/// Values come from `ksmedia.h` (`KSPROPERTY_VIDEOPROCAMP_POWERLINE_FREQUENCY`
/// uses 0 = disabled, 1 = 50 Hz, 2 = 60 Hz, 3 = auto; boolean properties use
/// 0 = off, 1 = on).
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
    /// Restore the driver's default value, in Auto mode where supported.
    ///
    /// This is what the Default button of the standard property dialog does.
    Default,
}

/// Parses a user-supplied value such as `50Hz`, `On`, `Auto` or `-5`.
///
/// Labels are checked before the `Auto` keyword, so a property whose label
/// table contains `Auto` (powerline frequency) gets that value rather than a
/// mode switch. Everything else that is not a label must be a decimal number.
///
/// # Errors
/// Fails when the string is neither a known label, `Auto`, nor a decimal
/// number that fits in an `i32`.
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
// Device data
// ---------------------------------------------------------------------------

/// Range, default and capability flags reported by `GetRange`.
#[derive(Debug, Clone, Copy, Default)]
struct PropertyRange {
    min: i32,
    max: i32,
    step: i32,
    default: i32,
    caps: i32,
}

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

/// Plain data describing a device and its supported properties.
///
/// Holds no COM interfaces, so it may outlive the [`ComSession`].
#[derive(Debug, Clone)]
pub(crate) struct DeviceInfo {
    /// The friendly name, or `"Unknown"` when the driver did not provide one.
    pub name: String,
    /// DirectShow device path, e.g. `\\?\usb#vid_046d&pid_082d&mi_00#...`.
    pub device_path: Option<String>,
    /// All supported properties, `VideoProcAmp` first, in query order.
    pub properties: Vec<PropertyInfo>,
}

impl DeviceInfo {
    /// Looks a property up, or `None` when the device does not support it.
    pub(crate) fn property(&self, property: Property) -> Option<&PropertyInfo> {
        self.properties.iter().find(|p| p.property == property)
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

/// What a successful write sent to the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Written {
    pub value: i32,
    pub mode: Mode,
}

impl Written {
    /// Whether a value read back from the device shows this write took effect.
    ///
    /// Manual writes must read back the same value; Auto writes only need the
    /// Auto flag, since the driver then chooses the value.
    pub(crate) const fn persisted_in(self, current: CurrentValue) -> bool {
        match self.mode {
            Mode::Manual => current.value == self.value,
            Mode::Auto => current.is_auto(),
        }
    }
}

/// How a write fared once it was read back through a fresh handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Persistence {
    /// The device reports the written value.
    Applied,
    /// The device reverted, but the UVC class driver stored the value for the
    /// next device start.
    Stored(CurrentValue),
    /// The device reverted and nothing stored the value.
    Dropped(CurrentValue),
    /// The device could not be read back (bind failed or the driver refused).
    Unverified,
}

/// The result of one accepted write, after read-back and any requested restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WriteReport {
    pub written: Written,
    pub persistence: Persistence,
    /// The device was restarted before the final read-back.
    pub restarted: bool,
}

/// One entry per job: the driver rejected the write, or what happened after it
/// was accepted.
pub(crate) type WriteOutcome = Result<WriteReport>;

/// How long to wait for a restarted camera to re-enumerate before giving up.
const DEVICE_RESTART_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Device handle
// ---------------------------------------------------------------------------

/// A capture device bound for the lifetime of a [`ComSession`].
///
/// The borrowed session guarantees the moniker is released before COM shuts
/// down; `info` holds the plain data.
#[derive(Debug)]
pub(crate) struct Device<'com> {
    moniker: IMoniker,
    pub info: DeviceInfo,
    _com: PhantomData<&'com ComSession>,
}

impl Device<'_> {
    /// Writes every job, then verifies the accepted writes through one fresh handle.
    ///
    /// Some drivers keep a value only while an application has the camera
    /// open; such a write counts as [`Persistence::Dropped`] unless the class
    /// driver stored it for the next device start ([`Persistence::Stored`]).
    /// With `restart`, one stored write is enough to restart the device once
    /// and read those writes again. Outcomes are returned in job order.
    ///
    /// # Errors
    /// Fails only when a requested device restart cannot be performed or the
    /// device does not come back afterwards.
    #[instrument(skip_all, fields(device = %self.info.name, jobs = jobs.len()))]
    pub(crate) fn write_all(
        &self,
        jobs: &[(&PropertyInfo, ParsedValue)],
        restart: bool,
    ) -> Result<Vec<WriteOutcome>> {
        let mut outcomes: Vec<WriteOutcome> = jobs
            .iter()
            .map(|&(info, value)| {
                self.set(info, value)
                    .inspect(|written| info!(property = %info.property, ?written, "Property set"))
                    .inspect_err(|error| {
                        debug!(property = %info.property, ?value, %error, "Failed to set property");
                    })
                    .map(|written| WriteReport {
                        written,
                        persistence: Persistence::Unverified,
                        restarted: false,
                    })
            })
            .collect();

        let accepted: Vec<usize> = (0..jobs.len()).filter(|&i| outcomes[i].is_ok()).collect();
        if accepted.is_empty() {
            return Ok(outcomes);
        }
        let properties: Vec<Property> = accepted.iter().map(|&i| jobs[i].0.property).collect();
        match self.read_back(&properties) {
            Ok(readings) => {
                for (&i, reading) in accepted.iter().zip(readings) {
                    if let Ok(report) = &mut outcomes[i] {
                        report.persistence =
                            self.classify(jobs[i].0.property, report.written, reading);
                    }
                }
            }
            Err(error) => debug!(%error, "Could not read values back"),
        }

        let pending: Vec<usize> = accepted
            .into_iter()
            .filter(|&i| {
                matches!(
                    outcomes[i],
                    Ok(WriteReport {
                        persistence: Persistence::Stored(_),
                        ..
                    })
                )
            })
            .collect();
        if !restart || pending.is_empty() {
            return Ok(outcomes);
        }

        info!("Restarting device to apply stored values");
        self.restart()?;
        let properties: Vec<Property> = pending.iter().map(|&i| jobs[i].0.property).collect();
        let readings = self.read_back_when_ready(&properties, DEVICE_RESTART_TIMEOUT)?;
        for (&i, reading) in pending.iter().zip(readings) {
            if let Ok(report) = &mut outcomes[i] {
                report.restarted = true;
                report.persistence = match reading {
                    Some(current) if report.written.persisted_in(current) => Persistence::Applied,
                    Some(current) => Persistence::Dropped(current),
                    None => Persistence::Unverified,
                };
            }
        }
        Ok(outcomes)
    }

    /// Classifies one read-back against what was written.
    fn classify(
        &self,
        property: Property,
        written: Written,
        reading: Option<CurrentValue>,
    ) -> Persistence {
        let Some(current) = reading else {
            return Persistence::Unverified;
        };
        if written.persisted_in(current) {
            return Persistence::Applied;
        }
        // The UVC class driver stores some controls and applies them at the
        // next device start even when the camera drops them on close; that is
        // a success with a caveat.
        if self.stored_value(property) == Some(written.value) {
            debug!(%property, ?written, ?current, "Write stored by the driver; pending device restart");
            Persistence::Stored(current)
        } else {
            debug!(%property, ?written, ?current, "Write did not persist");
            Persistence::Dropped(current)
        }
    }

    /// Writes a property on this device and reports what was sent.
    ///
    /// `info` is one of this device's own [`DeviceInfo::properties`]; the value
    /// is validated against its range and capabilities before the driver is
    /// called. A successful return means the driver accepted the write; use
    /// [`Device::read_back`] to check that it persisted.
    ///
    /// # Errors
    /// Fails when the value is out of range, the requested mode is not
    /// supported, or the driver rejects the write.
    #[instrument(skip(self, info), fields(property = %info.property))]
    fn set(&self, info: &PropertyInfo, value: ParsedValue) -> Result<Written> {
        let written = resolve_set(info, value)?;

        let filter = bind_filter(&self.moniker)?;
        control_set(&filter, info.property, written.value, written.mode.flag()).with_context(
            || {
                format!(
                    "Failed to set {} to {} ({})",
                    info.property, written.value, written.mode
                )
            },
        )?;
        debug!(value = written.value, mode = %written.mode, "Property set");
        Ok(written)
    }

    /// Reads properties back through a fresh device handle.
    ///
    /// Some drivers keep a written value only while an application holds the
    /// camera open and revert it when the last handle closes. Re-binding the
    /// filter after the write is the only way to observe that from a single
    /// process. Returns one entry per requested property; `None` when the
    /// driver refused to read it.
    ///
    /// # Errors
    /// Fails only when the device cannot be bound at all.
    fn read_back(&self, properties: &[Property]) -> Result<Vec<Option<CurrentValue>>> {
        let filter = bind_filter(&self.moniker)?;
        properties
            .iter()
            .map(|&property| control_get(&filter, property))
            .collect()
    }

    /// [`Device::read_back`], retried until the device answers or `timeout` passes.
    ///
    /// After a restart the device takes a moment to re-enumerate; binding fails
    /// until then.
    ///
    /// # Errors
    /// Returns the last bind error once the timeout has elapsed.
    fn read_back_when_ready(
        &self,
        properties: &[Property],
        timeout: Duration,
    ) -> Result<Vec<Option<CurrentValue>>> {
        let start = Instant::now();
        loop {
            match self.read_back(properties) {
                Ok(readings) => return Ok(readings),
                Err(error) if start.elapsed() < timeout => {
                    trace!(%error, "Device not ready yet; retrying");
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(error) => {
                    return Err(error.context("Device did not come back after the restart"));
                }
            }
        }
    }

    /// The value the UVC class driver has stored for a property, if any.
    ///
    /// `usbvideo.sys` records some controls under the device's
    /// `Device Parameters` registry key when they are written (currently only
    /// `PowerlineFrequency` is known to be stored) and applies them the next
    /// time the device starts. A camera that does not keep such a control
    /// across handle closes therefore still honours the write after a
    /// reconnect or reboot. Returns `None` for vendor drivers, properties the
    /// class driver does not store, or devices without a device path.
    fn stored_value(&self, property: Property) -> Option<i32> {
        if property != Property::PowerlineFrequency {
            return None;
        }
        let instance = device_instance_id(self.info.device_path.as_deref()?)?;
        read_device_parameter_dword(&instance, "PowerlineFrequency")
    }

    /// Restarts the device (disable, then enable) so stored values take effect.
    ///
    /// This is what `pnputil /restart-device` does. The camera disappears for
    /// a moment and any application using it loses the stream.
    ///
    /// # Errors
    /// Fails without administrator rights (`CR_ACCESS_DENIED`), when the
    /// device has no usable device path, or when Configuration Manager
    /// rejects the operation.
    #[instrument(skip(self))]
    fn restart(&self) -> Result<()> {
        let path = self
            .info
            .device_path
            .as_deref()
            .context("Device path not available; cannot restart the device")?;
        let instance = device_instance_id(path)
            .context("Could not derive a device instance id from the device path")?;
        let instance_w = HSTRING::from(instance.as_str());

        let mut devinst = 0u32;
        // SAFETY: `devinst` is a valid out-slot and `instance_w` a
        // NUL-terminated wide string; both outlive the call.
        let cr =
            unsafe { CM_Locate_DevNodeW(&raw mut devinst, &instance_w, CM_LOCATE_DEVNODE_NORMAL) };
        check_configret(cr, "locate the device")?;

        debug!(instance, devinst, "Restarting device");
        // SAFETY: plain call taking the device instance handle located above.
        let disabled = unsafe { CM_Disable_DevNode(devinst, 0) };
        check_configret(disabled, "disable the device")?;
        // SAFETY: as above.
        let enabled = unsafe { CM_Enable_DevNode(devinst, 0) };
        check_configret(enabled, "enable the device")?;
        Ok(())
    }

    /// Opens the driver's own property dialog (the "Video Proc Amp" and
    /// "Camera Control" pages) and blocks until the user closes it.
    ///
    /// This is the same window OBS Studio and other DirectShow hosts show for
    /// "Configure Video": the filter's `ISpecifyPropertyPages` pages, displayed
    /// with `OleCreatePropertyFrame`. Changes made in the dialog are written by
    /// the driver's page, not by this tool.
    ///
    /// # Errors
    /// Fails when the device cannot be bound, exposes no property pages, or
    /// the frame cannot be created.
    #[instrument(skip(self), fields(device = %self.info.name))]
    pub(crate) fn open_property_dialog(&self) -> Result<()> {
        let filter = bind_filter(&self.moniker)?;
        let pages: ISpecifyPropertyPages = filter
            .cast()
            .context("Device does not expose property pages")?;
        // SAFETY: `pages` is a live interface. The returned CAUUID owns a
        // CoTaskMem allocation that is freed below on every path.
        let page_ids = unsafe { pages.GetPages() }.context("Failed to enumerate property pages")?;
        let free_pages = || {
            // SAFETY: `pElems` was allocated by COM for us and is freed
            // exactly once; a null pointer is a no-op.
            unsafe { CoTaskMemFree(Some(page_ids.pElems.cast_const().cast())) };
        };
        if page_ids.cElems == 0 || page_ids.pElems.is_null() {
            free_pages();
            bail!("Device has no property pages");
        }

        let object: Option<IUnknown> = Some(filter.cast().context("Failed to get IUnknown")?);
        let caption = HSTRING::from(self.info.name.as_str());
        debug!(pages = page_ids.cElems, "Opening property dialog");
        // SAFETY: `object` is a valid one-element array of live interface
        // pointers; `pElems` points at `cElems` valid CLSIDs; the caption is a
        // NUL-terminated wide string that outlives the call. The frame runs
        // its own modal message loop on this STA thread and returns when the
        // dialog closes.
        let result = unsafe {
            OleCreatePropertyFrame(
                HWND::default(),
                0,
                0,
                &caption,
                1,
                &raw const object,
                page_ids.cElems,
                page_ids.pElems,
                0,
                None,
                None,
            )
        };
        free_pages();
        result.context("Failed to open the property dialog")
    }
}

/// Maps a Configuration Manager status to an error with a readable reason.
fn check_configret(cr: CONFIGRET, what: &str) -> Result<()> {
    if cr == CR_SUCCESS {
        Ok(())
    } else if cr == CR_ACCESS_DENIED {
        bail!("Failed to {what}: access denied (this needs an elevated prompt)")
    } else {
        bail!("Failed to {what} (CONFIGRET {})", cr.0)
    }
}

/// Whether this process runs with administrator rights.
///
/// `IsUserAnAdmin` checks the process token for the Administrators group.
/// Under UAC a non-elevated administrator runs with a filtered token in which
/// that group is deny-only, so the check answers false until the process is
/// elevated, which is exactly what `--restart-device` needs to know.
#[must_use]
pub(crate) fn is_elevated() -> bool {
    // SAFETY: plain FFI call with no arguments.
    unsafe { IsUserAnAdmin() }.as_bool()
}

/// Converts a DirectShow device path into a PnP device instance id.
///
/// `\\?\usb#vid_046d&pid_082d&mi_00#6&1f335e1e&1&0000#{guid}\global`
/// becomes `USB\VID_046D&PID_082D&MI_00\6&1F335E1E&1&0000`, which is the
/// device's key under `HKLM\SYSTEM\CurrentControlSet\Enum`.
fn device_instance_id(device_path: &str) -> Option<String> {
    let path = device_path.strip_prefix("\\\\?\\").unwrap_or(device_path);
    let (instance, _interface_class) = path.split_once("#{")?;
    let id = instance.replace('#', "\\").to_ascii_uppercase();
    (id.matches('\\').count() == 2).then_some(id)
}

/// Reads a DWORD from `HKLM\SYSTEM\CurrentControlSet\Enum\<instance>\Device Parameters`.
///
/// This key is world-readable, so no elevation is needed.
fn read_device_parameter_dword(instance_id: &str, value_name: &str) -> Option<i32> {
    let key = windows_registry::LOCAL_MACHINE
        .open(format!(
            "SYSTEM\\CurrentControlSet\\Enum\\{instance_id}\\Device Parameters"
        ))
        .ok()?;
    match key.get_u32(value_name) {
        Ok(data) => i32::try_from(data).ok(),
        Err(error) => {
            trace!(instance_id, value_name, %error, "No stored device parameter");
            None
        }
    }
}

/// Turns a requested value into the value and mode the driver will be sent.
///
/// `Auto` keeps the current value (or the default) so drivers that insist on an
/// in-range value even in auto mode are satisfied. `Default` restores the
/// driver's default and re-enables Auto where the property supports it, which
/// is what the standard property dialog's Default button does. A property that
/// advertises capabilities is only switched to a mode it supports; a property
/// reporting no capabilities at all is written in manual mode as before.
fn resolve_set(info: &PropertyInfo, value: ParsedValue) -> Result<Written> {
    let name = info.property;
    match value {
        ParsedValue::Auto => {
            if !Mode::Auto.is_supported(info.caps) {
                bail!(
                    "Property '{name}' does not support Auto mode (supported modes: {})",
                    format_capabilities(info.caps).unwrap_or_else(|| "none".to_owned())
                );
            }
            let keep = info.current.map_or(info.default, |c| c.value);
            Ok(Written {
                value: keep,
                mode: Mode::Auto,
            })
        }
        ParsedValue::Default => {
            let mode = if Mode::Auto.is_supported(info.caps) {
                Mode::Auto
            } else {
                Mode::Manual
            };
            Ok(Written {
                value: info.default,
                mode,
            })
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
            Ok(Written {
                value: v,
                mode: Mode::Manual,
            })
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
    const KIND: PropertyType;

    fn range(&self, id: i32) -> windows::core::Result<PropertyRange>;
    fn get(&self, id: i32) -> windows::core::Result<CurrentValue>;
    fn set(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()>;
}

/// Implements [`PropertyControl`] for an interface with that method triple.
///
/// windows-rs generates identical signatures for both interfaces, so the
/// bodies are the same; only the type and its [`PropertyType`] differ.
macro_rules! impl_property_control {
    ($interface:ty, $kind:expr) => {
        impl PropertyControl for $interface {
            const KIND: PropertyType = $kind;

            fn range(&self, id: i32) -> windows::core::Result<PropertyRange> {
                let mut r = PropertyRange::default();
                // SAFETY: `self` is a live interface; every out-pointer refers
                // to a local `i32` that outlives the call.
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
                let mut c = CurrentValue::default();
                // SAFETY: `self` is a live interface; both out-pointers refer
                // to local `i32`s that outlive the call.
                unsafe { self.Get(id, &raw mut c.value, &raw mut c.flags) }?;
                Ok(c)
            }

            fn set(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()> {
                // SAFETY: `self` is a live interface; the arguments are plain
                // integers.
                unsafe { self.Set(id, value, flags) }
            }
        }
    };
}

impl_property_control!(IAMVideoProcAmp, PropertyType::VideoProcAmp);
impl_property_control!(IAMCameraControl, PropertyType::CameraControl);

/// Casts a bound filter to one of the property interfaces.
fn interface<C: PropertyControl>(filter: &IBaseFilter) -> Result<C> {
    filter
        .cast()
        .with_context(|| format!("Device does not expose the {:?} interface", C::KIND))
}

/// Dispatches a write to the interface the property belongs to.
fn control_set(filter: &IBaseFilter, property: Property, value: i32, flags: i32) -> Result<()> {
    let id = property.id();
    match property.kind() {
        PropertyType::VideoProcAmp => interface::<IAMVideoProcAmp>(filter)?.set(id, value, flags),
        PropertyType::CameraControl => interface::<IAMCameraControl>(filter)?.set(id, value, flags),
    }
    .map_err(Into::into)
}

/// Dispatches a read to the interface the property belongs to.
///
/// Returns `None` when the driver refuses to read the value.
fn control_get(filter: &IBaseFilter, property: Property) -> Result<Option<CurrentValue>> {
    let id = property.id();
    Ok(match property.kind() {
        PropertyType::VideoProcAmp => interface::<IAMVideoProcAmp>(filter)?.get(id).ok(),
        PropertyType::CameraControl => interface::<IAMCameraControl>(filter)?.get(id).ok(),
    })
}

/// Queries every property of one interface that the filter supports.
///
/// A property whose `GetRange` fails is treated as unsupported and skipped; a
/// property whose `Get` fails is reported without a current value.
fn query_properties<C: PropertyControl>(filter: &IBaseFilter) -> Result<Vec<PropertyInfo>> {
    let iface: C = interface(filter)?;

    let mut properties = Vec::new();
    for property in Property::ALL.into_iter().filter(|p| p.kind() == C::KIND) {
        let Ok(range) = iface.range(property.id()) else {
            trace!(%property, "GetRange failed; property not supported");
            continue;
        };
        let current = iface.get(property.id()).ok();
        trace!(%property, ?range, ?current, "Property queried");
        properties.push(PropertyInfo {
            property,
            min: range.min,
            max: range.max,
            step: range.step,
            default: range.default,
            caps: range.caps,
            current,
        });
    }
    debug!(kind = ?C::KIND, count = properties.len(), "Properties enumerated");
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
            name: friendly_name(moniker),
            device_path: read_bag_string(moniker, "DevicePath").ok(),
        })
        .collect())
}

/// Enumerates capture devices and reads every supported property of each.
///
/// Each device is bound to its filter once; a device that cannot be bound is
/// still returned, with an empty property list, so indices stay stable between
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
        let name = friendly_name(&moniker);
        let device_path = read_bag_string(&moniker, "DevicePath").ok();
        debug!(%name, ?device_path, "Processing device");

        let properties = match bind_filter(&moniker) {
            Ok(filter) => {
                let mut properties =
                    query_properties::<IAMVideoProcAmp>(&filter).unwrap_or_default();
                properties
                    .extend(query_properties::<IAMCameraControl>(&filter).unwrap_or_default());
                properties
            }
            Err(error) => {
                debug!(%name, %error, "Could not bind device filter; reporting no properties");
                Vec::new()
            }
        };

        devices.push(Device {
            moniker,
            info: DeviceInfo {
                name,
                device_path,
                properties,
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

/// The device's friendly name, or `"Unknown"` when the driver provides none.
fn friendly_name(moniker: &IMoniker) -> String {
    read_bag_string(moniker, "FriendlyName").unwrap_or_else(|_| "Unknown".to_owned())
}

/// Reads a string-valued entry (`FriendlyName`, `DevicePath`) from a device's property bag.
fn read_bag_string(moniker: &IMoniker, property: &str) -> Result<String> {
    // SAFETY: `moniker` is a live interface; the result type is checked
    // against its IID.
    let bag: IPropertyBag = unsafe { moniker.BindToStorage(None, None) }
        .with_context(|| format!("Failed to bind property bag for '{property}'"))?;

    let name = HSTRING::from(property);
    // windows-rs's `VARIANT` clears itself on drop, whatever `Read` stores in it.
    let mut var = VARIANT::default();
    // SAFETY: `bag` is a live interface; `name` is a valid NUL-terminated wide
    // string that outlives the call; `var` is a valid, initialised VARIANT
    // out-parameter; no error log is supplied.
    unsafe { bag.Read(&name, &raw mut var, None) }
        .with_context(|| format!("Failed to read property '{property}'"))?;

    let value = BSTR::try_from(&var)
        .with_context(|| format!("Property '{property}' is not a string ({:?})", var.vt()))?
        .to_string();
    trace!(property, value = %value, "Property bag entry read");
    Ok(value)
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
        assert_eq!(
            parse_property_value(Property::PowerlineFrequency, "Auto").unwrap(),
            ParsedValue::Manual(3)
        );
        assert_eq!(
            parse_property_value(Property::PowerlineFrequency, "AUTO").unwrap(),
            ParsedValue::Manual(3)
        );
    }

    #[test]
    fn auto_keyword_requests_auto_mode_elsewhere() {
        assert_eq!(
            parse_property_value(Property::Brightness, "auto").unwrap(),
            ParsedValue::Auto
        );
        assert_eq!(
            parse_property_value(Property::Exposure, "Auto").unwrap(),
            ParsedValue::Auto
        );
        assert_eq!(
            parse_property_value(Property::ColorEnable, "auto").unwrap(),
            ParsedValue::Auto
        );
    }

    #[test]
    fn numeric_values_parse_including_negatives() {
        assert_eq!(
            parse_property_value(Property::Exposure, "-5").unwrap(),
            ParsedValue::Manual(-5)
        );
        assert_eq!(
            parse_property_value(Property::Brightness, "128").unwrap(),
            ParsedValue::Manual(128)
        );
        assert_eq!(
            parse_property_value(Property::PowerlineFrequency, "1").unwrap(),
            ParsedValue::Manual(1)
        );
    }

    #[test]
    fn invalid_values_are_rejected() {
        parse_property_value(Property::Brightness, "abc").unwrap_err();
        parse_property_value(Property::Brightness, "1 2").unwrap_err();
        parse_property_value(Property::Brightness, "").unwrap_err();
        parse_property_value(Property::Brightness, "99999999999").unwrap_err();
        // Arabic-Indic digit three: a Unicode digit, but not a decimal number.
        parse_property_value(Property::Brightness, "\u{663}").unwrap_err();
        parse_property_value(Property::Brightness, "50Hz").unwrap_err();
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
        let manual = |value| Written {
            value,
            mode: Mode::Manual,
        };
        let auto = |value| Written {
            value,
            mode: Mode::Auto,
        };
        assert_eq!(
            resolve_set(&p, ParsedValue::Manual(-5)).unwrap(),
            manual(-5)
        );
        resolve_set(&p, ParsedValue::Manual(0)).unwrap_err();
        resolve_set(&p, ParsedValue::Manual(-12)).unwrap_err();
        // Auto keeps the default when no current value is known.
        assert_eq!(resolve_set(&p, ParsedValue::Auto).unwrap(), auto(-6));
        // ...and the current value when it is.
        let mut live = p.clone();
        live.current = Some(CurrentValue {
            value: -3,
            flags: Mode::Manual.flag(),
        });
        assert_eq!(resolve_set(&live, ParsedValue::Auto).unwrap(), auto(-3));

        let manual_only = info(Property::Brightness, 0, 255, 1, 128, Mode::Manual.flag());
        let err = resolve_set(&manual_only, ParsedValue::Auto).unwrap_err();
        assert!(err.to_string().contains("Manual"), "{err}");

        let auto_only = info(Property::Focus, 0, 255, 5, 0, Mode::Auto.flag());
        resolve_set(&auto_only, ParsedValue::Manual(10)).unwrap_err();

        // No capabilities reported at all: keep writing manual values as before.
        let no_caps = info(Property::Gamma, 100, 300, 1, 200, 0);
        assert_eq!(
            resolve_set(&no_caps, ParsedValue::Manual(150)).unwrap(),
            manual(150)
        );
    }

    #[test]
    fn default_restores_the_default_value_in_auto_where_supported() {
        let both = Mode::Auto.flag() | Mode::Manual.flag();
        let auto_capable = info(Property::WhiteBalance, 2000, 6500, 1, 4000, both);
        assert_eq!(
            resolve_set(&auto_capable, ParsedValue::Default).unwrap(),
            Written {
                value: 4000,
                mode: Mode::Auto
            }
        );
        let manual_only = info(Property::Brightness, 0, 255, 1, 128, Mode::Manual.flag());
        assert_eq!(
            resolve_set(&manual_only, ParsedValue::Default).unwrap(),
            Written {
                value: 128,
                mode: Mode::Manual
            }
        );
        let no_caps = info(Property::PowerlineFrequency, 1, 2, 1, 2, 0);
        assert_eq!(
            resolve_set(&no_caps, ParsedValue::Default).unwrap(),
            Written {
                value: 2,
                mode: Mode::Manual
            }
        );
    }

    #[test]
    fn persistence_check_compares_value_for_manual_and_flag_for_auto() {
        let manual = Written {
            value: 2,
            mode: Mode::Manual,
        };
        assert!(manual.persisted_in(CurrentValue { value: 2, flags: 0 }));
        assert!(!manual.persisted_in(CurrentValue { value: 1, flags: 0 }));
        let auto = Written {
            value: 4000,
            mode: Mode::Auto,
        };
        assert!(auto.persisted_in(CurrentValue {
            value: 3534,
            flags: Mode::Auto.flag()
        }));
        assert!(!auto.persisted_in(CurrentValue {
            value: 4000,
            flags: Mode::Manual.flag()
        }));
    }

    #[test]
    fn device_path_maps_to_pnp_instance_id() {
        let path = "\\\\?\\usb#vid_046d&pid_082d&mi_00#6&1f335e1e&1&0000#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\\global";
        assert_eq!(
            device_instance_id(path).as_deref(),
            Some("USB\\VID_046D&PID_082D&MI_00\\6&1F335E1E&1&0000")
        );
        assert_eq!(device_instance_id("not a device path"), None);
        assert_eq!(device_instance_id("\\\\?\\usb#only-one-part#{guid}"), None);
    }

    #[test]
    fn device_info_lookup_reports_unsupported_properties() {
        let device = DeviceInfo {
            name: "Unknown".to_owned(),
            device_path: None,
            properties: vec![
                info(Property::Brightness, 0, 255, 1, 128, 2),
                info(Property::Focus, 0, 255, 1, 0, 3),
            ],
        };
        assert!(device.property(Property::Brightness).is_some());
        assert!(device.property(Property::Focus).is_some());
        assert!(device.property(Property::Zoom).is_none());
    }
}
