//! DirectShow webcam enumeration and property control.
//!
//! This module is the only place that talks to Windows: `ICreateDevEnum` to
//! enumerate capture devices, `IPropertyBag` for their names and paths, and
//! `IAMVideoProcAmp` / `IAMCameraControl` to read and write properties. The
//! pure property logic (identifiers, modes, labels, parsing) lives in
//! [`property`] and is re-exported here.
//!
//! Every function that touches COM takes a [`ComSession`], the proof that
//! `CoInitializeEx` succeeded on this thread. [`Device`] handles borrow it, so
//! no COM interface can outlive the session. The apartment is single-threaded
//! and nothing is marshalled or called back, so it needs no message pump.
//!
//! Property writes go straight to the driver and persist across processes,
//! exactly like changes made through the Windows camera dialog.

mod property;

use std::marker::PhantomData;
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
    IAMCameraControl, IAMVideoProcAmp, IBaseFilter, ICreateDevEnum,
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

use property::resolve_set;
pub(crate) use property::{
    CurrentValue, Mode, ParsedValue, Property, PropertyInfo, PropertyType, Written,
    build_enum_display, current_mode, format_capabilities, format_property_value,
    parse_property_value,
};

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
    use super::*;

    fn info(property: Property) -> PropertyInfo {
        PropertyInfo {
            property,
            min: 0,
            max: 255,
            step: 1,
            default: 0,
            caps: 0,
            current: None,
        }
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
            properties: vec![info(Property::Brightness), info(Property::Focus)],
        };
        assert!(device.property(Property::Brightness).is_some());
        assert!(device.property(Property::Focus).is_some());
        assert!(device.property(Property::Zoom).is_none());
    }
}
