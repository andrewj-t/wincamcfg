//! DirectShow webcam enumeration and property control.
//!
//! This module is the only place that talks to Windows: `ICreateDevEnum` to
//! enumerate capture devices, `IPropertyBag` for their names and paths, and
//! `IAMVideoProcAmp` / `IAMCameraControl` to read and write properties.
//!
//! Every function that touches COM takes a [`ComSession`], the proof that
//! `CoInitializeEx` succeeded on this thread. [`Device`] handles borrow it, so
//! no COM interface can outlive the session. The apartment is single-threaded
//! and nothing is marshalled or called back, so it needs no message pump.
//!
//! Property writes go straight to the driver and persist across processes,
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
/// The raw-pointer marker makes it `!Send + !Sync`, so `CoUninitialize` runs
/// on the thread that called `CoInitializeEx`.
#[derive(Debug)]
pub(crate) struct ComSession(PhantomData<*const ()>);

impl ComSession {
    /// Initialises a single-threaded apartment; `S_FALSE` (already initialised) counts as success.
    pub(crate) fn new() -> Result<Self> {
        debug!("Initializing COM");
        // SAFETY: plain FFI call; every success is paired with `CoUninitialize` in `Drop`.
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
        // SAFETY: pairs with `new()` on the same thread; every borrower has already dropped.
        unsafe { CoUninitialize() };
    }
}

// ---------------------------------------------------------------------------
// DirectShow identifiers
// ---------------------------------------------------------------------------

/// `CLSID_SystemDeviceEnum` from `uuids.h`.
const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);

/// `CLSID_VideoInputDeviceCategory` from `uuids.h`.
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

/// Plain data describing a device; holds no COM interfaces.
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
    /// Whether a read-back shows this write took effect: same value for Manual,
    /// the Auto flag for Auto (the driver then chooses the value).
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
    /// The device reverted, but the UVC class driver stored the value for the next device start.
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

/// One entry per job: the driver rejected the write, or what happened after it was accepted.
pub(crate) type WriteOutcome = Result<WriteReport>;

/// How long to wait for a restarted camera to re-enumerate before giving up.
const DEVICE_RESTART_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Device handle
// ---------------------------------------------------------------------------

/// A capture device bound for the lifetime of a [`ComSession`].
#[derive(Debug)]
pub(crate) struct Device<'com> {
    moniker: IMoniker,
    pub info: DeviceInfo,
    _com: PhantomData<&'com ComSession>,
}

impl Device<'_> {
    /// Writes every job, then verifies the accepted writes through one fresh handle.
    ///
    /// A write the camera drops on close is [`Persistence::Dropped`] unless the
    /// class driver stored it ([`Persistence::Stored`]); with `restart`, any
    /// stored write restarts the device once and reads those writes again.
    /// Fails only when that restart cannot be performed. Outcomes are in job order.
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
        let mut pending = Vec::new();
        match self.read_back(&properties) {
            Ok(readings) => {
                for (&i, reading) in accepted.iter().zip(readings) {
                    if let Ok(report) = &mut outcomes[i] {
                        report.persistence =
                            self.classify(jobs[i].0.property, report.written, reading);
                        if matches!(report.persistence, Persistence::Stored(_)) {
                            pending.push(i);
                        }
                    }
                }
            }
            Err(error) => debug!(%error, "Could not read values back"),
        }
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
            Persistence::Applied
        } else if self.stored_value(property) == Some(written.value) {
            debug!(%property, ?written, ?current, "Write stored by the driver; pending device restart");
            Persistence::Stored(current)
        } else {
            debug!(%property, ?written, ?current, "Write did not persist");
            Persistence::Dropped(current)
        }
    }

    /// Writes one of this device's properties; success means the driver accepted it.
    #[instrument(skip(self, info), fields(property = %info.property))]
    fn set(&self, info: &PropertyInfo, value: ParsedValue) -> Result<Written> {
        let written = resolve_set(info, value)?;
        let filter = bind_filter(&self.moniker)?;
        control_set(&filter, info.property, written).with_context(|| {
            format!(
                "Failed to set {} to {} ({})",
                info.property, written.value, written.mode
            )
        })?;
        Ok(written)
    }

    /// Reads properties back through a fresh device handle.
    ///
    /// Some drivers keep a written value only while an application holds the
    /// camera open and revert it when the last handle closes; re-binding the
    /// filter after the write is the only way to observe that. `None` means the
    /// driver refused to read that property.
    fn read_back(&self, properties: &[Property]) -> Result<Vec<Option<CurrentValue>>> {
        let filter = bind_filter(&self.moniker)?;
        properties
            .iter()
            .map(|&property| control_get(&filter, property))
            .collect()
    }

    /// [`Device::read_back`], retried until the device re-enumerates or `timeout` passes.
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

    /// The value `usbvideo.sys` stored under the device's `Device Parameters` key, if any.
    ///
    /// The class driver records some controls (only `PowerlineFrequency` is
    /// known) when they are written and applies them at the next device start.
    fn stored_value(&self, property: Property) -> Option<i32> {
        if property != Property::PowerlineFrequency {
            return None;
        }
        let instance = device_instance_id(self.info.device_path.as_deref()?)?;
        read_device_parameter_dword(&instance, "PowerlineFrequency")
    }

    /// Restarts the device (disable, then enable), as `pnputil /restart-device` does.
    ///
    /// Needs administrator rights and interrupts any application using the camera.
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
        // SAFETY: `devinst` is a valid out-slot and `instance_w` a NUL-terminated wide string.
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

    /// Opens the driver's own property pages (OBS's "Configure Video" window) and blocks until closed.
    ///
    /// Changes made there are written by the driver's page, not by this tool.
    #[instrument(skip(self), fields(device = %self.info.name))]
    pub(crate) fn open_property_dialog(&self) -> Result<()> {
        let filter = bind_filter(&self.moniker)?;
        let pages: ISpecifyPropertyPages = filter
            .cast()
            .context("Device does not expose property pages")?;
        // SAFETY: `pages` is a live interface; the returned CAUUID is freed below on every path.
        let page_ids = unsafe { pages.GetPages() }.context("Failed to enumerate property pages")?;
        let free_pages = || {
            // SAFETY: COM allocated `pElems` for us; freed exactly once, and null is a no-op.
            unsafe { CoTaskMemFree(Some(page_ids.pElems.cast_const().cast())) };
        };
        if page_ids.cElems == 0 || page_ids.pElems.is_null() {
            free_pages();
            bail!("Device has no property pages");
        }
        let object: Option<IUnknown> = Some(filter.cast().context("Failed to get IUnknown")?);
        let caption = HSTRING::from(self.info.name.as_str());
        debug!(pages = page_ids.cElems, "Opening property dialog");
        // SAFETY: `object` is a one-element array of live interfaces, `pElems` holds `cElems`
        // CLSIDs and `caption` outlives the call; the frame's modal loop runs on this STA thread.
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
/// `IsUserAnAdmin` checks the process token for the Administrators group; under
/// UAC a non-elevated administrator's filtered token fails that check, which is
/// exactly what `--restart-device` needs to know.
#[must_use]
pub(crate) fn is_elevated() -> bool {
    // SAFETY: plain FFI call with no arguments.
    unsafe { IsUserAnAdmin() }.as_bool()
}

/// Converts a DirectShow device path into the PnP instance id used as its registry key:
/// `\\?\usb#vid_046d&pid_082d&mi_00#6&1f335e1e&1&0000#{guid}\global` becomes
/// `USB\VID_046D&PID_082D&MI_00\6&1F335E1E&1&0000`.
fn device_instance_id(device_path: &str) -> Option<String> {
    let path = device_path.strip_prefix("\\\\?\\").unwrap_or(device_path);
    let (instance, _interface_class) = path.split_once("#{")?;
    let id = instance.replace('#', "\\").to_ascii_uppercase();
    (id.matches('\\').count() == 2).then_some(id)
}

/// Reads a DWORD from the device's `Device Parameters` key, which is world-readable.
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

/// Turns a requested value into the value and mode sent to the driver.
///
/// `Auto` keeps the current (or default) value for drivers that insist on an
/// in-range value; `Default` re-enables Auto where supported, like the dialog's
/// Default button. A property reporting no capabilities is written in manual mode.
fn resolve_set(info: &PropertyInfo, value: ParsedValue) -> Result<Written> {
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
// Property interface abstraction
// ---------------------------------------------------------------------------

/// Common shape of `IAMVideoProcAmp` and `IAMCameraControl`, so one query routine serves both.
trait PropertyControl: Interface {
    const KIND: PropertyType;

    fn range(&self, id: i32) -> windows::core::Result<PropertyRange>;
    fn get(&self, id: i32) -> windows::core::Result<CurrentValue>;
    fn set(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()>;
}

/// windows-rs generates identical method signatures for both interfaces.
macro_rules! impl_property_control {
    ($interface:ty, $kind:expr) => {
        impl PropertyControl for $interface {
            const KIND: PropertyType = $kind;

            fn range(&self, id: i32) -> windows::core::Result<PropertyRange> {
                let mut r = PropertyRange::default();
                // SAFETY: `self` is a live interface; the out-pointers are locals outliving the call.
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
                // SAFETY: `self` is a live interface; the out-pointers are locals outliving the call.
                unsafe { self.Get(id, &raw mut c.value, &raw mut c.flags) }?;
                Ok(c)
            }

            fn set(&self, id: i32, value: i32, flags: i32) -> windows::core::Result<()> {
                // SAFETY: `self` is a live interface; the arguments are plain integers.
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
fn control_set(filter: &IBaseFilter, property: Property, written: Written) -> Result<()> {
    let (id, value, flags) = (property.id(), written.value, written.mode.flag());
    match property.kind() {
        PropertyType::VideoProcAmp => interface::<IAMVideoProcAmp>(filter)?.set(id, value, flags),
        PropertyType::CameraControl => interface::<IAMCameraControl>(filter)?.set(id, value, flags),
    }
    .map_err(Into::into)
}

/// Dispatches a read; `None` when the driver refuses to read the value.
fn control_get(filter: &IBaseFilter, property: Property) -> Result<Option<CurrentValue>> {
    let id = property.id();
    Ok(match property.kind() {
        PropertyType::VideoProcAmp => interface::<IAMVideoProcAmp>(filter)?.get(id).ok(),
        PropertyType::CameraControl => interface::<IAMCameraControl>(filter)?.get(id).ok(),
    })
}

/// Queries every property of one interface; a failing `GetRange` means unsupported.
fn query_properties<C: PropertyControl>(filter: &IBaseFilter) -> Result<Vec<PropertyInfo>> {
    let iface: C = interface(filter)?;
    let properties: Vec<PropertyInfo> = Property::ALL
        .into_iter()
        .filter(|p| p.kind() == C::KIND)
        .filter_map(|property| {
            let range = iface
                .range(property.id())
                .inspect_err(|_| trace!(%property, "GetRange failed; property not supported"))
                .ok()?;
            let current = iface.get(property.id()).ok();
            trace!(%property, ?range, ?current, "Property queried");
            Some(PropertyInfo {
                property,
                min: range.min,
                max: range.max,
                step: range.step,
                default: range.default,
                caps: range.caps,
                current,
            })
        })
        .collect();
    debug!(kind = ?C::KIND, count = properties.len(), "Properties enumerated");
    Ok(properties)
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

/// Lists capture devices by name and path without binding their filters, so a
/// driver that stalls when bound cannot stall `list`.
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
/// A device that cannot be bound is still returned with no properties, so
/// indices stay stable between `list` and `get`.
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
    // SAFETY: COM is initialised on this thread (a `ComSession` is borrowed); the CLSID is valid.
    let dev_enum: ICreateDevEnum =
        unsafe { CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER) }
            .context("Failed to create system device enumerator")?;
    let mut enum_moniker: Option<IEnumMoniker> = None;
    // SAFETY: `dev_enum` is a live interface; the GUID and out-slot are valid for the call.
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
        // SAFETY: live interface, valid one-element out-slice and out-pointer; S_FALSE ends it.
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
    // SAFETY: `moniker` is a live interface; no bind context or left moniker is required.
    unsafe { moniker.BindToObject(None, None) }.context("Failed to bind to device filter")
}

/// The device's friendly name, or `"Unknown"` when the driver provides none.
fn friendly_name(moniker: &IMoniker) -> String {
    read_bag_string(moniker, "FriendlyName").unwrap_or_else(|_| "Unknown".to_owned())
}

/// Reads a string-valued entry (`FriendlyName`, `DevicePath`) from a device's property bag.
fn read_bag_string(moniker: &IMoniker, property: &str) -> Result<String> {
    // SAFETY: `moniker` is a live interface; the result type is checked against its IID.
    let bag: IPropertyBag = unsafe { moniker.BindToStorage(None, None) }
        .with_context(|| format!("Failed to bind property bag for '{property}'"))?;
    let name = HSTRING::from(property);
    // windows-rs's `VARIANT` clears itself on drop, whatever `Read` stores in it.
    let mut var = VARIANT::default();
    // SAFETY: `bag` is live; `name` and `var` are valid for the call; no error log is supplied.
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
