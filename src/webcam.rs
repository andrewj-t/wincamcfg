/// Webcam and DirectShow interaction module.
///
/// This module handles all DirectShow COM interactions for webcam device enumeration,
/// property querying and setting, and device information retrieval. It provides a domain
/// layer abstraction over Windows DirectShow APIs with type-safe property enums and
/// value formatting.
///
/// # Windows APIs
///
/// Uses the following DirectShow/COM interfaces:
/// - `ICreateDevEnum` — enumerates device categories
/// - `IEnumMoniker` — iterates through device monikers
/// - `IMoniker` — represents device identity and binding point
/// - `IPropertyBag` — reads device metadata (name, path)
/// - `IBaseFilter` — device filter interface
/// - `IAMVideoProcAmp` — video processing properties (brightness, contrast, etc.)
/// - `IAMCameraControl` — camera control properties (exposure, focus, etc.)
///
/// # Debugging
///
/// Set `RUST_LOG=trace` to see detailed COM initialization and property enumeration traces.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString};
use tracing::{debug, instrument, trace};
use windows::{
    Win32::Foundation::*, Win32::Media::DirectShow::*,
    Win32::System::Com::StructuredStorage::IPropertyBag, Win32::System::Com::*, core::*,
};

/// RAII guard for COM initialization/cleanup.
///
/// COM interfaces (ICreateDevEnum, IEnumMoniker, IMoniker, IPropertyBag, etc.)
/// are automatically cleaned up when they go out of scope via their Drop implementations
/// in the windows-rs crate. This guard only handles CoInitialize/CoUninitialize.
struct ComGuard;

impl ComGuard {
    /// Initializes COM for apartment-threaded use on the current thread.
    ///
    /// # Safety
    ///
    /// Callers must ensure this is called from a thread that has not already initialized
    /// COM with an incompatible threading model. Calling from multiple threads is safe
    /// as each thread has its own COM apartment.
    ///
    /// # Errors
    ///
    /// Returns an error if COM initialization fails with a code other than `S_FALSE`.
    unsafe fn new() -> Result<Self> {
        // SAFETY: FFI call; safe as long as called once per thread. Subsequent calls on
        // the same thread return S_FALSE which is treated as success. See documentation.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // S_OK (0) = initialized, S_FALSE (1) = already initialized
        // Both are considered success for our purposes
        if hr.is_err() {
            return Err(anyhow::anyhow!("Failed to initialize COM: {:?}", hr));
        }
        Ok(ComGuard)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: FFI call; safe because ComGuard is only constructed via new(),
        // pairing is enforced by RAII.
        unsafe {
            CoUninitialize();
        }
    }
}

/// System Device Enumerator class ID.
///
/// Source: DirectShow SDK, `ksmedia.h`, `CLSID_SystemDeviceEnum`.
const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);

/// Video input device filter category GUID.
///
/// Source: DirectShow SDK, `ksmedia.h`, `CLSID_VideoInputDeviceCategory`.
const CLSID_VIDEO_INPUT_DEVICE_CATEGORY: GUID =
    GUID::from_u128(0x860bb310_5d01_11d0_bd3b_00a0c911ce86);

/// VideoProcAmp property IDs from DirectShow (ksmedia.h)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString)]
#[repr(i32)]
pub enum VideoProcAmpProperty {
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

impl From<VideoProcAmpProperty> for i32 {
    fn from(property: VideoProcAmpProperty) -> Self {
        property as i32
    }
}

/// CameraControl property IDs from DirectShow (ksmedia.h)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString)]
#[repr(i32)]
pub enum CameraControlProperty {
    Pan = 0,
    Tilt = 1,
    Roll = 2,
    Zoom = 3,
    Exposure = 4,
    Iris = 5,
    Focus = 6,
}

impl From<CameraControlProperty> for i32 {
    fn from(property: CameraControlProperty) -> Self {
        property as i32
    }
}

/// Property type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display)]
pub enum PropertyType {
    VideoProcAmp,
    CameraControl,
}

/// Device information including metadata and all available properties.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: Option<String>,
    pub device_path: Option<String>,
    pub video_proc_amp_properties: Vec<PropertyInfo>,
    pub camera_control_properties: Vec<PropertyInfo>,
}

/// Property information with current value, defaults, and supported values
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PropertyInfo {
    pub name: String,
    pub min: Option<i32>,
    pub max: Option<i32>,
    pub step: Option<i32>,
    pub default: Option<i32>,
    pub caps: Option<i32>,
    pub current: Option<i32>,
    pub capabilities: Option<String>,
    pub property_type: PropertyType,
}

/// Returns the static value↔label table for enum-like properties.
/// Each entry is (numeric_value, canonical_label).
fn get_value_labels(property_name: &str) -> Option<&'static [(i32, &'static str)]> {
    match property_name {
        "PowerlineFrequency" => Some(&[(0, "Disabled"), (1, "50Hz"), (2, "60Hz"), (3, "Auto")]),
        "ColorEnable" | "BacklightCompensation" => Some(&[(0, "Off"), (1, "On")]),
        _ => None,
    }
}

/// Format a property value into a human-readable label based on the property name
pub fn format_property_value(property_name: &str, value: i32) -> String {
    if let Some(labels) = get_value_labels(property_name) {
        return labels
            .iter()
            .find(|&&(v, _)| v == value)
            .map(|&(_, label)| label.to_string())
            .unwrap_or_else(|| format!("Unknown({})", value));
    }
    value.to_string()
}

/// Build enum mapping from property name and min/max for display
pub fn build_enum_display(property_name: &str, min: i32, max: i32) -> Option<String> {
    let labels = get_value_labels(property_name)?;
    let display = labels
        .iter()
        .filter(|&&(v, _)| v >= min && v <= max)
        .map(|&(val, label)| format!("{} ({})", label, val))
        .collect::<Vec<_>>()
        .join(", ");
    Some(display).filter(|s| !s.is_empty())
}

/// Parse a value string to (value, auto_mode) tuple.
///
/// Handles both human-readable values (50Hz, On, Off, Auto) and numeric values.
/// Returns Ok((value, true)) if "Auto" is specified.
/// Returns Ok((parsed_value, false)) for other inputs.
///
/// # Errors
///
/// Returns an error if the input exceeds 32 characters, contains invalid characters, or parsing fails.
pub fn parse_property_value(property_name: &str, value_str: &str) -> Result<(i32, bool)> {
    // Sanitize input: limit length to prevent potential issues
    if value_str.len() > 32 {
        anyhow::bail!("Value string exceeds maximum allowed length");
    }

    // Sanitize input: only allow alphanumeric characters and specific safe characters
    if !value_str
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == ' ')
    {
        anyhow::bail!("Value contains invalid characters");
    }

    // Check if Auto mode is requested
    if value_str.eq_ignore_ascii_case("auto") {
        return Ok((0, true)); // Value doesn't matter when auto is true
    }

    // For enum-like properties: try label match first, then numeric parse
    if let Some(labels) = get_value_labels(property_name) {
        if let Some(&(v, _)) = labels
            .iter()
            .find(|&&(_, l)| l.eq_ignore_ascii_case(value_str))
        {
            return Ok((v, false));
        }
        // Numeric parse with a helpful hint listing valid labels
        let valid = labels
            .iter()
            .map(|&(_, l)| l)
            .collect::<Vec<_>>()
            .join(", ");
        let v = value_str.parse::<i32>().with_context(|| {
            format!(
                "Invalid value '{}' for {}. Expected one of: {}, or a number",
                value_str, property_name, valid
            )
        })?;
        return Ok((v, false));
    }

    // Generic numeric parse for non-enum properties
    let v = value_str
        .parse::<i32>()
        .with_context(|| format!("Invalid numeric value '{}'", value_str))?;
    Ok((v, false))
}

/// Simplified device list item for list command.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceListItem {
    pub index: usize,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_path: Option<String>,
}

/// List all video capture devices (lightweight - names and paths only).
///
/// This is a simplified version of `enumerate_devices()` for the list command.
///
/// # Errors
///
/// Returns an error if device enumeration fails.
#[instrument]
pub fn list_devices() -> Result<Vec<DeviceListItem>> {
    debug!("Listing video capture devices");

    let devices = enumerate_devices()?;

    Ok(devices
        .into_iter()
        .enumerate()
        .map(|(index, device)| DeviceListItem {
            index,
            name: device.name.unwrap_or_else(|| "Unknown".to_string()),
            device_path: device.device_path,
        })
        .collect())
}

/// Enumerate all video capture devices and return their information.
///
/// # Errors
///
/// Returns an error if COM initialization fails or if the device enumerator cannot be created.
#[instrument]
pub fn enumerate_devices() -> Result<Vec<DeviceInfo>> {
    // SAFETY: All COM interfaces are obtained from DirectShow; lifetimes managed by windows-rs Drop.
    unsafe {
        debug!("Initializing COM");
        let _com = ComGuard::new()?;
        debug!("COM initialized successfully");

        debug!("Creating ICreateDevEnum");
        // Create the System Device Enumerator
        let dev_enum: ICreateDevEnum =
            CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER)
                .context("Failed to create device enumerator")?;
        debug!("ICreateDevEnum created successfully");

        debug!("Creating class enumerator for video input devices");
        // Create an enumerator for the video input device category
        let mut enum_moniker: Option<IEnumMoniker> = None;
        let hr = dev_enum.CreateClassEnumerator(
            &CLSID_VIDEO_INPUT_DEVICE_CATEGORY,
            &mut enum_moniker,
            0,
        );
        debug!("CreateClassEnumerator returned: {:?}", hr);
        hr.context("Failed to create video device class enumerator")?;

        let Some(enum_moniker) = enum_moniker else {
            debug!("No video devices found (enum_moniker is None)");
            return Ok(Vec::new());
        };

        let mut devices = Vec::new();

        debug!("Starting device enumeration loop");
        loop {
            let mut monikers: [Option<IMoniker>; 1] = [None];
            let mut fetched = 0u32;

            trace!("Calling enum_moniker.Next()");
            let hr = enum_moniker.Next(&mut monikers, Some(&mut fetched));

            if hr != S_OK || fetched == 0 {
                debug!("Enumeration complete. HR: {:?}, Fetched: {}", hr, fetched);
                break;
            }

            trace!("Processing moniker {} (fetched={})", devices.len(), fetched);

            if let Some(mon) = &monikers[0] {
                trace!("Binding moniker to property bag");
                let device_name = get_device_name(mon).ok();
                debug!(device_name = ?device_name, "Processing device");

                let mut device = DeviceInfo {
                    name: device_name,
                    device_path: None,
                    video_proc_amp_properties: Vec::new(),
                    camera_control_properties: Vec::new(),
                };

                // Get device path
                trace!("Getting device path");
                if let Ok(path) = get_device_path(mon) {
                    trace!(device_path = %path, "Device path obtained");
                    device.device_path = Some(path);
                }

                // Get VideoProcAmp properties
                trace!("Querying VideoProcAmp properties");
                if let Ok(props) = get_video_proc_amp_properties(mon) {
                    debug!(
                        property_count = props.len(),
                        "VideoProcAmp properties enumerated"
                    );
                    device.video_proc_amp_properties = props;
                } else {
                    trace!("Failed to get VideoProcAmp properties");
                }

                // Get CameraControl properties
                trace!("Querying CameraControl properties");
                if let Ok(props) = get_camera_control_properties(mon) {
                    debug!(
                        property_count = props.len(),
                        "CameraControl properties enumerated"
                    );
                    device.camera_control_properties = props;
                } else {
                    trace!("Failed to get CameraControl properties");
                }

                debug!(
                    device_name = ?device.name,
                    video_proc_amp_count = device.video_proc_amp_properties.len(),
                    camera_control_count = device.camera_control_properties.len(),
                    "Device enumeration complete"
                );
                devices.push(device);
            }
        }

        Ok(devices)
    }
}

#[instrument(skip(moniker))]
fn get_device_name(moniker: &IMoniker) -> Result<String> {
    debug!("Reading FriendlyName from property bag");
    let result = get_property_string(moniker, "FriendlyName");
    if let Ok(ref name) = result {
        debug!(friendly_name = %name, "Device name obtained");
    } else {
        trace!("Failed to read FriendlyName");
    }
    result
}

#[instrument(skip(moniker))]
fn get_device_path(moniker: &IMoniker) -> Result<String> {
    debug!("Reading DevicePath from property bag");
    let result = get_property_string(moniker, "DevicePath");
    if let Ok(ref path) = result {
        trace!(device_path = %path, "Device path obtained");
    } else {
        trace!("Failed to read DevicePath");
    }
    result
}

#[instrument(skip(moniker))]
fn get_property_string(moniker: &IMoniker, prop_name: &str) -> Result<String> {
    trace!(property_name = %prop_name, "Binding moniker to property bag");
    use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VariantClear};
    use windows::core::HSTRING;

    // SAFETY: moniker is a valid IMoniker obtained from DirectShow enumeration.
    let prop_bag: IPropertyBag =
        unsafe { moniker.BindToStorage(None, None) }.with_context(|| {
            format!(
                "Failed to bind to property bag for property '{}'",
                prop_name
            )
        })?;
    trace!("BindToStorage successful");

    let mut var = VARIANT::default();
    let prop_name_hstr = HSTRING::from(prop_name);

    // SAFETY: prop_bag is a valid IPropertyBag; var is default-initialized VARIANT.
    unsafe { prop_bag.Read(PCWSTR(prop_name_hstr.as_ptr()), &mut var, None) }
        .with_context(|| format!("Failed to read property '{}'", prop_name))?;

    // Extract the value
    // SAFETY: Only accessed after confirming vt == VT_BSTR.
    let result = if unsafe { var.Anonymous.Anonymous.vt } == VT_BSTR {
        // SAFETY: Only accessed after confirming vt == VT_BSTR.
        let bstr = unsafe { &var.Anonymous.Anonymous.Anonymous.bstrVal };
        let value = bstr.to_string();
        trace!(property_value = %value, "Property value retrieved");
        Ok(value)
    } else {
        Err(anyhow::anyhow!("Property '{}' is not a BSTR", prop_name))
    };

    // SAFETY: Must be called to free BSTR regardless of success; var is a local stack-allocated VARIANT.
    let _ = unsafe { VariantClear(&mut var) };

    result
}

// Convert capability flags to human-readable string ("Manual", "Auto", "Manual, Auto")
fn format_capabilities(caps: i32) -> Option<String> {
    let mut cap_names = Vec::new();

    if caps & VideoProcAmp_Flags_Manual.0 != 0 {
        cap_names.push("Manual");
    }
    if caps & VideoProcAmp_Flags_Auto.0 != 0 {
        cap_names.push("Auto");
    }

    if cap_names.is_empty() {
        None
    } else {
        Some(cap_names.join(", "))
    }
}

fn get_properties<T, IFace, GetRangeFn, GetFn>(
    moniker: &IMoniker,
    iface_cast: fn(IBaseFilter) -> Result<IFace>,
    properties: &[T],
    get_range: GetRangeFn,
    get_value: GetFn,
    property_type: PropertyType,
) -> Result<Vec<PropertyInfo>>
where
    T: Copy + ToString + Into<i32>,
    IFace: Clone,
    GetRangeFn: Fn(
        &IFace,
        i32,
        &mut i32,
        &mut i32,
        &mut i32,
        &mut i32,
        &mut i32,
    ) -> windows::core::Result<()>,
    GetFn: Fn(&IFace, i32, &mut i32, &mut i32) -> windows::core::Result<()>,
{
    debug!("Binding moniker to IBaseFilter");
    // SAFETY: moniker is a valid IMoniker; IBaseFilter is the standard bind target.
    let filter: IBaseFilter =
        unsafe { moniker.BindToObject(None, None) }.context("Failed to bind to IBaseFilter")?;
    let iface = iface_cast(filter)?;
    debug!("Interface obtained");

    let mut capabilities = Vec::new();
    trace!(property_count = properties.len(), "Enumerating properties");
    for property in properties {
        let prop_id: i32 = (*property).into();
        let name = property.to_string();
        let mut min = 0;
        let mut max = 0;
        let mut step = 0;
        let mut default = 0;
        let mut caps = 0;

        if get_range(
            &iface,
            prop_id,
            &mut min,
            &mut max,
            &mut step,
            &mut default,
            &mut caps,
        )
        .is_ok()
        {
            trace!(property = %name, min, max, step, default, caps, "GetRange successful");
            let mut value = 0;
            let mut flags_val = 0;
            if get_value(&iface, prop_id, &mut value, &mut flags_val).is_ok() {
                trace!(property = %name, value, flags = flags_val, "Get successful");
            }

            capabilities.push(PropertyInfo {
                name: name.to_string(),
                min: Some(min),
                max: Some(max),
                step: Some(step),
                default: Some(default),
                caps: Some(caps),
                current: Some(value),
                capabilities: format_capabilities(caps),
                property_type,
            });
        } else {
            trace!(property = %name, "GetRange failed - property not supported");
        }
    }
    debug!(
        property_count = capabilities.len(),
        "Property enumeration complete"
    );
    Ok(capabilities)
}

/// Get VideoProcAmp properties from a device moniker.
///
/// # Errors
///
/// Returns an error if the interface cannot be obtained or device enumeration fails.
unsafe fn get_video_proc_amp_properties(moniker: &IMoniker) -> Result<Vec<PropertyInfo>> {
    // SAFETY: Interfaces are validly obtained; property IDs are from the VideoProcAmpProperty enum.
    get_properties(
        moniker,
        |f| f.cast().context("Failed to get IAMVideoProcAmp interface"),
        &[
            VideoProcAmpProperty::Brightness,
            VideoProcAmpProperty::Contrast,
            VideoProcAmpProperty::Saturation,
            VideoProcAmpProperty::Hue,
            VideoProcAmpProperty::WhiteBalance,
            VideoProcAmpProperty::WhiteBalanceComponent,
            VideoProcAmpProperty::ColorEnable,
            VideoProcAmpProperty::Gamma,
            VideoProcAmpProperty::Sharpness,
            VideoProcAmpProperty::BacklightCompensation,
            VideoProcAmpProperty::Gain,
            VideoProcAmpProperty::PowerlineFrequency,
            VideoProcAmpProperty::DigitalMultiplier,
            VideoProcAmpProperty::DigitalMultiplierLimit,
        ],
        |iface: &IAMVideoProcAmp, prop_id, min, max, step, default, caps| unsafe {
            // SAFETY: iface is a valid IAMVideoProcAmp; arguments are mutable refs to valid i32s.
            iface.GetRange(prop_id, min, max, step, default, caps)
        },
        |iface: &IAMVideoProcAmp, prop_id, value, flags| unsafe {
            // SAFETY: iface is a valid IAMVideoProcAmp; arguments are mutable refs to valid i32s.
            iface.Get(prop_id, value, flags)
        },
        PropertyType::VideoProcAmp,
    )
}

/// Get CameraControl properties from a device moniker.
///
/// # Errors
///
/// Returns an error if the interface cannot be obtained or device enumeration fails.
unsafe fn get_camera_control_properties(moniker: &IMoniker) -> Result<Vec<PropertyInfo>> {
    // SAFETY: Interfaces are validly obtained; property IDs are from the CameraControlProperty enum.
    get_properties(
        moniker,
        |f| f.cast().context("Failed to get IAMCameraControl interface"),
        &[
            CameraControlProperty::Exposure,
            CameraControlProperty::Focus,
            CameraControlProperty::Pan,
            CameraControlProperty::Tilt,
            CameraControlProperty::Roll,
            CameraControlProperty::Zoom,
            CameraControlProperty::Iris,
        ],
        |iface: &IAMCameraControl, prop_id, min, max, step, default, caps| unsafe {
            // SAFETY: iface is a valid IAMCameraControl; arguments are mutable refs to valid i32s.
            iface.GetRange(prop_id, min, max, step, default, caps)
        },
        |iface: &IAMCameraControl, prop_id, value, flags| unsafe {
            // SAFETY: iface is a valid IAMCameraControl; arguments are mutable refs to valid i32s.
            iface.Get(prop_id, value, flags)
        },
        PropertyType::CameraControl,
    )
}

/// Find a device moniker by its DirectShow device path.
///
/// Used by set_property functions to locate the device for property modification.
///
/// # Errors
///
/// Returns an error if no devices are found or the target device path is not found.
unsafe fn find_device_by_path(target_path: &str) -> Result<IMoniker> {
    // SAFETY: COM is caller-initialized (called from with_device_filter).
    let dev_enum: ICreateDevEnum =
        unsafe { CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER)? };

    let mut enum_moniker: Option<IEnumMoniker> = None;
    // SAFETY: dev_enum is valid; enum_moniker is a valid mutable ref.
    unsafe {
        dev_enum.CreateClassEnumerator(&CLSID_VIDEO_INPUT_DEVICE_CATEGORY, &mut enum_moniker, 0)?
    };

    let Some(enum_moniker) = enum_moniker else {
        anyhow::bail!("No video devices found");
    };

    loop {
        let mut monikers: [Option<IMoniker>; 1] = [None];
        let mut fetched = 0u32;

        // SAFETY: enum_moniker is valid; monikers and fetched are valid mutable refs.
        let hr = unsafe { enum_moniker.Next(&mut monikers, Some(&mut fetched)) };

        if hr != S_OK || fetched == 0 {
            break;
        }

        if let Some(mon) = monikers[0].take()
            && let Ok(path) = get_device_path(&mon)
            && path == target_path
        {
            return Ok(mon);
        }
    }

    anyhow::bail!("Device not found")
}

/// Shared boilerplate: initialize COM, find device, bind to IBaseFilter, invoke closure.
///
/// The ComGuard lives for the duration of the closure so COM stays initialized.
fn with_device_filter<R, F>(device: &DeviceInfo, f: F) -> Result<R>
where
    F: FnOnce(IBaseFilter) -> Result<R>,
{
    // SAFETY: Thread-safe initialization; RAII ensures CoUninitialize is called.
    let _com = unsafe { ComGuard::new()? };

    let target_path = device
        .device_path
        .as_ref()
        .context("Device path not available")?;

    // SAFETY: COM is initialized in the line above; target path is validated by caller.
    let mon = unsafe { find_device_by_path(target_path)? };
    // SAFETY: mon was obtained from a valid DirectShow enumerator.
    let filter: IBaseFilter =
        unsafe { mon.BindToObject(None, None) }.context("Failed to bind to device filter")?;

    f(filter)
}

/// Set a VideoProcAmp property on a device.
///
/// # Errors
///
/// Returns an error if the device path is missing, the interface cannot be obtained, or the set operation fails.
pub fn set_video_proc_amp_property(
    device: &DeviceInfo,
    property: VideoProcAmpProperty,
    value: i32,
    auto: bool,
) -> Result<()> {
    with_device_filter(device, |filter| {
        let iface: IAMVideoProcAmp = filter
            .cast()
            .context("Failed to get VideoProcAmp interface")?;
        let flags = if auto {
            VideoProcAmp_Flags_Auto.0
        } else {
            VideoProcAmp_Flags_Manual.0
        };
        // SAFETY: iface is obtained via valid cast(); property.into() maps to the DirectShow enum value.
        unsafe { iface.Set(property.into(), value, flags) }.with_context(|| {
            format!(
                "Failed to set VideoProcAmp property {} to value {}",
                property, value
            )
        })
    })
}

/// Set a CameraControl property on a device.
///
/// # Errors
///
/// Returns an error if the device path is missing, the interface cannot be obtained, or the set operation fails.
pub fn set_camera_control_property(
    device: &DeviceInfo,
    property: CameraControlProperty,
    value: i32,
    auto: bool,
) -> Result<()> {
    with_device_filter(device, |filter| {
        let iface: IAMCameraControl = filter
            .cast()
            .context("Failed to get CameraControl interface")?;
        let flags = if auto {
            CameraControl_Flags_Auto.0
        } else {
            CameraControl_Flags_Manual.0
        };
        // SAFETY: iface is obtained via valid cast(); property.into() maps to the DirectShow enum value.
        unsafe { iface.Set(property.into(), value, flags) }.with_context(|| {
            format!(
                "Failed to set CameraControl property {} to value {}",
                property, value
            )
        })
    })
}

/// Set a property by name on a device.
///
/// High-level function that:
/// - Parses the value string (handles Auto, 50Hz, On/Off, etc.)
/// - Validates the property exists and value is within safe ranges
/// - Determines if it's a VideoProcAmp or `CameraControl` property
/// - Calls the appropriate low-level setter function
///
/// # Errors
///
/// Returns an error if the property name is invalid, contains non-alphanumeric characters,
/// exceeds 64 characters, is not found on the device, the value is out of range,
/// or the set operation fails.
pub fn set_property(device: &DeviceInfo, property_name: &str, value_str: &str) -> Result<()> {
    // Sanitize property name - only allow alphanumeric characters
    if !property_name.chars().all(char::is_alphanumeric) {
        anyhow::bail!("Invalid property name: contains non-alphanumeric characters");
    }

    // Limit property name length to prevent potential issues
    if property_name.len() > 64 {
        anyhow::bail!("Invalid property name: exceeds maximum length");
    }

    // Parse the value string to get numeric value and auto flag
    let (numeric_value, auto_mode) = parse_property_value(property_name, value_str)?;

    // Try to find the property in VideoProcAmp properties first
    let property_info = device
        .video_proc_amp_properties
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(property_name))
        .map(|p| (p, PropertyType::VideoProcAmp))
        .or_else(|| {
            device
                .camera_control_properties
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(property_name))
                .map(|p| (p, PropertyType::CameraControl))
        });

    let (prop_info, property_type) = property_info.ok_or_else(|| {
        anyhow::anyhow!(
            "Property '{}' not found on device '{}'",
            property_name,
            device.name.as_deref().unwrap_or("Unknown")
        )
    })?;

    // Validate value is within safe range (skip validation for auto mode)
    if !auto_mode
        && let (Some(min), Some(max)) = (prop_info.min, prop_info.max)
        && (numeric_value < min || numeric_value > max)
    {
        anyhow::bail!(
            "Value {} for property '{}' is outside the supported range [{}, {}]",
            numeric_value,
            property_name,
            min,
            max
        );
    }

    match property_type {
        PropertyType::VideoProcAmp => {
            #[expect(
                clippy::map_err_ignore,
                reason = "parse error is not useful; we provide domain context"
            )]
            let prop_enum: VideoProcAmpProperty = property_name
                .parse()
                .map_err(|_| anyhow::anyhow!("Unknown VideoProcAmp property: {}", property_name))?;
            set_video_proc_amp_property(device, prop_enum, numeric_value, auto_mode)
        }
        PropertyType::CameraControl => {
            #[expect(
                clippy::map_err_ignore,
                reason = "parse error is not useful; we provide domain context"
            )]
            let prop_enum: CameraControlProperty = property_name.parse().map_err(|_| {
                anyhow::anyhow!("Unknown CameraControl property: {}", property_name)
            })?;
            set_camera_control_property(device, prop_enum, numeric_value, auto_mode)
        }
    }
}

// Rust guideline compliant 2026-02-21

/// Trait abstraction for camera device operations.
///
/// Enables testable code by allowing implementations to either use real DirectShow APIs
/// or provide mockable test fixtures. Following M-MOCKABLE-SYSCALLS from Microsoft Rust guidelines.
pub trait CameraDevice: Clone {
    /// Retrieve device information (name and path).
    fn get_device_info(&self) -> Result<(Option<String>, Option<String>)>;
    /// Get all VideoProcAmp properties from the device.
    fn get_video_proc_amp_properties(&self) -> Result<Vec<PropertyInfo>>;
    /// Get all CameraControl properties from the device.
    fn get_camera_control_properties(&self) -> Result<Vec<PropertyInfo>>;
    /// Set a VideoProcAmp property to the specified value.
    fn set_video_proc_amp_property(
        &mut self,
        property: VideoProcAmpProperty,
        value: i32,
        auto: bool,
    ) -> Result<()>;
    /// Set a CameraControl property to the specified value.
    fn set_camera_control_property(
        &mut self,
        property: CameraControlProperty,
        value: i32,
        auto: bool,
    ) -> Result<()>;
}

/// Mockable camera device for testing (feature-gated).
#[cfg(feature = "test-util")]
#[derive(Clone, Debug)]
pub struct MockCamera {
    pub name: Option<String>,
    pub device_path: Option<String>,
    pub video_proc_amp_properties: Vec<PropertyInfo>,
    pub camera_control_properties: Vec<PropertyInfo>,
}

#[cfg(feature = "test-util")]
impl MockCamera {
    /// Create mock camera with default properties.
    pub fn new(name: Option<String>, device_path: Option<String>) -> Self {
        let mut camera = Self {
            name,
            device_path,
            video_proc_amp_properties: Vec::new(),
            camera_control_properties: Vec::new(),
        };
        camera.video_proc_amp_properties = Self::default_video_proc_amp();
        camera.camera_control_properties = Self::default_camera_control();
        camera
    }

    /// Set a mock property's current value.
    pub fn set_property_value(&mut self, property_name: &str, value: i32) -> Result<()> {
        for prop in &mut self.video_proc_amp_properties {
            if prop.name.eq_ignore_ascii_case(property_name) {
                prop.current = Some(value);
                return Ok(());
            }
        }
        for prop in &mut self.camera_control_properties {
            if prop.name.eq_ignore_ascii_case(property_name) {
                prop.current = Some(value);
                return Ok(());
            }
        }
        Err(anyhow::anyhow!("Property not found: {}", property_name))
    }

    fn default_video_proc_amp() -> Vec<PropertyInfo> {
        vec![
            PropertyInfo {
                name: "Brightness".to_string(),
                min: Some(0),
                max: Some(255),
                step: Some(1),
                default: Some(128),
                caps: Some(VideoProcAmp_Flags_Manual.0),
                current: Some(128),
                capabilities: Some("Manual".to_string()),
                property_type: PropertyType::VideoProcAmp,
            },
            PropertyInfo {
                name: "PowerlineFrequency".to_string(),
                min: Some(0),
                max: Some(3),
                step: Some(1),
                default: Some(0),
                caps: Some(VideoProcAmp_Flags_Auto.0 | VideoProcAmp_Flags_Manual.0),
                current: Some(2),
                capabilities: Some("Manual, Auto".to_string()),
                property_type: PropertyType::VideoProcAmp,
            },
        ]
    }

    fn default_camera_control() -> Vec<PropertyInfo> {
        vec![PropertyInfo {
            name: "Focus".to_string(),
            min: Some(0),
            max: Some(255),
            step: Some(1),
            default: Some(128),
            caps: Some(CameraControl_Flags_Manual.0),
            current: Some(128),
            capabilities: Some("Manual".to_string()),
            property_type: PropertyType::CameraControl,
        }]
    }
}

#[cfg(feature = "test-util")]
impl CameraDevice for MockCamera {
    fn get_device_info(&self) -> Result<(Option<String>, Option<String>)> {
        Ok((self.name.clone(), self.device_path.clone()))
    }
    fn get_video_proc_amp_properties(&self) -> Result<Vec<PropertyInfo>> {
        Ok(self.video_proc_amp_properties.clone())
    }
    fn get_camera_control_properties(&self) -> Result<Vec<PropertyInfo>> {
        Ok(self.camera_control_properties.clone())
    }
    fn set_video_proc_amp_property(
        &mut self,
        property: VideoProcAmpProperty,
        value: i32,
        _auto: bool,
    ) -> Result<()> {
        self.set_property_value(&property.to_string(), value)
    }
    fn set_camera_control_property(
        &mut self,
        property: CameraControlProperty,
        value: i32,
        _auto: bool,
    ) -> Result<()> {
        self.set_property_value(&property.to_string(), value)
    }
}
