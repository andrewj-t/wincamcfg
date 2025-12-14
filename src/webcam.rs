/// Webcam and DirectShow interaction module
///
/// This module handles all DirectShow COM interactions for webcam device enumeration,
/// property querying and setting, and device information retrieval. It provides a domain
/// layer abstraction over Windows DirectShow APIs with type-safe property enums and
/// value formatting.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString};
use tracing::{debug, instrument, trace};
use windows::{
    Win32::Devices::DeviceAndDriverInstallation::*, Win32::Foundation::*,
    Win32::Media::DirectShow::*, Win32::System::Com::StructuredStorage::*, Win32::System::Com::*,
    Win32::UI::Shell::*, core::*,
};

// String buffer size for Windows API calls (registry, property bags, etc.)
const STRING_BUFFER_SIZE: usize = 512;

/// RAII guard for COM initialization/cleanup
///
/// COM interfaces (ICreateDevEnum, IEnumMoniker, IMoniker, IPropertyBag, etc.)
/// are automatically cleaned up when they go out of scope via their Drop implementations
/// in the windows-rs crate. This guard only handles CoInitialize/CoUninitialize.
struct ComGuard;

impl ComGuard {
    unsafe fn new() -> Result<Self> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            // S_OK (0) = initialized, S_FALSE (1) = already initialized
            // Both are considered success for our purposes
            if hr.is_err() {
                return Err(anyhow::anyhow!("Failed to initialize COM: {:?}", hr));
            }
        }
        Ok(ComGuard)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

// DirectShow GUIDs for device enumeration
const CLSID_SYSTEM_DEVICE_ENUM: GUID = GUID::from_u128(0x62be5d10_60eb_11d0_bd3b_00a0c911ce86);
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

/// Device information including metadata and all available properties
#[derive(Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: Option<String>,
    pub device_path: Option<String>,
    pub device_description: Option<String>,
    pub manufacturer: Option<String>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub driver_path: Option<String>,
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

/// Build enum mapping from property name and min/max for display
pub fn build_enum_display(property_name: &str, min: i32, max: i32) -> Option<String> {
    let mapping = get_enum_mapping(property_name, min, max)?;
    let display = mapping
        .iter()
        .map(|(val, label)| format!("{} ({})", label, val))
        .collect::<Vec<_>>()
        .join(", ");
    Some(display)
}

/// Format a property value into a human-readable label based on the property name
pub fn format_property_value(property_name: &str, value: i32) -> String {
    match property_name {
        "PowerlineFrequency" => match value {
            0 => "Disabled".to_string(),
            1 => "50Hz".to_string(),
            2 => "60Hz".to_string(),
            3 => "Auto".to_string(),
            _ => format!("Unknown({})", value),
        },
        "ColorEnable" => match value {
            0 => "Off".to_string(),
            1 => "On".to_string(),
            _ => format!("Unknown({})", value),
        },
        "BacklightCompensation" => match value {
            0 => "Off".to_string(),
            1 => "On".to_string(),
            _ => format!("Unknown({})", value),
        },
        _ => value.to_string(),
    }
}

/// Parse a value string to (value, auto_mode) tuple
/// Handles both human-readable values (50Hz, On, Off, Auto) and numeric values
/// Returns Ok((value, true)) if "Auto" is specified
/// Returns Ok((parsed_value, false)) for other inputs
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

    // Parse property-specific string values
    let value = match property_name {
        "PowerlineFrequency" => match value_str {
            "Disabled" | "disabled" => 0,
            "50Hz" | "50hz" | "50" => 1,
            "60Hz" | "60hz" | "60" => 2,
            _ => value_str.parse::<i32>()
                .with_context(|| format!("Invalid value '{}' for PowerlineFrequency. Expected: Disabled, 50Hz, 60Hz, or Auto", value_str))?,
        },
        "ColorEnable" | "BacklightCompensation" => match value_str {
            "Off" | "off" | "0" => 0,
            "On" | "on" | "1" => 1,
            _ => value_str.parse::<i32>()
                .with_context(|| format!("Invalid value '{}'. Expected: Off, On, or Auto", value_str))?,
        },
        _ => value_str.parse::<i32>()
            .with_context(|| format!("Invalid numeric value '{}'", value_str))?,
    };

    Ok((value, false))
}

/// Internal structure for collecting driver information from Windows CM API
#[derive(Default)]
struct DriverInfo {
    device_desc: Option<String>,
    manufacturer: Option<String>,
    driver_version: Option<String>,
    driver_date: Option<String>,
    driver_path: Option<String>,
}

/// Simplified device list item for list command
#[derive(Serialize, Deserialize)]
pub struct DeviceListItem {
    pub index: usize,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_path: Option<String>,
}

/// List all video capture devices (lightweight - names and paths only)
/// This is a simplified version of enumerate_devices() for the list command
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

/// Enumerate all video capture devices and return their information
#[instrument]
pub fn enumerate_devices() -> Result<Vec<DeviceInfo>> {
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
                    device_description: None,
                    manufacturer: None,
                    driver_version: None,
                    driver_date: None,
                    driver_path: None,
                    video_proc_amp_properties: Vec::new(),
                    camera_control_properties: Vec::new(),
                };

                // Get device path
                trace!("Getting device path");
                if let Ok(path) = get_device_path(mon) {
                    trace!(device_path = %path, "Device path obtained");
                    device.device_path = Some(path.clone());

                    // Get driver information
                    if let Ok(driver_info) = get_driver_info_from_path(&path) {
                        device.device_description = driver_info.device_desc;
                        device.manufacturer = driver_info.manufacturer;
                        device.driver_version = driver_info.driver_version;
                        device.driver_date = driver_info.driver_date;
                        device.driver_path = driver_info.driver_path;
                    }
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

    // Bind to property bag (safe)
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

    // Read property value (unsafe only for FFI call)
    unsafe { prop_bag.Read(PCWSTR(prop_name_hstr.as_ptr()), &mut var, None) }
        .with_context(|| format!("Failed to read property '{}'", prop_name))?;

    // Extract the value (safe)
    let result = if unsafe { var.Anonymous.Anonymous.vt } == VT_BSTR {
        // Use windows::core::BSTR for conversion
        let bstr = unsafe { &var.Anonymous.Anonymous.Anonymous.bstrVal };
        let value = bstr.to_string();
        trace!(property_value = %value, "Property value retrieved");
        Ok(value)
    } else {
        Err(anyhow::anyhow!("Property '{}' is not a BSTR", prop_name))
    };

    // Always clear VARIANT regardless of success or failure (unsafe only for FFI call)
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

// Extract Windows device instance ID from DirectShow device path
// Converts path like "\\?\usb#vid_046d&pid_082d..." to "USB\VID_046D&PID_082D\..."
fn parse_device_instance_id(device_path: &str) -> Option<String> {
    let path_lower = device_path.to_lowercase();
    let usb_start = path_lower.find("usb#")?;
    let path_part = &device_path[usb_start + 4..];
    let guid_start = path_part.find("#{")?;
    let device_id = &path_part[..guid_start];
    let parts: Vec<&str> = device_id.split('#').collect();
    if parts.len() >= 2 {
        return Some(format!("USB\\{}\\{}", parts[0].to_uppercase(), parts[1]));
    }
    None
}

unsafe fn get_driver_info_from_path(device_path: &str) -> Result<DriverInfo> {
    let device_instance_id = match parse_device_instance_id(device_path) {
        Some(id) => id,
        None => return Ok(DriverInfo::default()),
    };

    unsafe {
        let device_id_wide: Vec<u16> = device_instance_id
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut devinst: u32 = 0;
        let cr = CM_Locate_DevNodeW(
            &mut devinst,
            PCWSTR(device_id_wide.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        );

        if cr != CR_SUCCESS {
            return Ok(DriverInfo::default());
        }

        let (driver_version, driver_date, driver_path) = get_driver_info_from_cm(devinst)
            .ok()
            .unwrap_or((None, None, None));

        let driver_info = DriverInfo {
            device_desc: get_cm_property_string(devinst, CM_DRP_DEVICEDESC).ok(),
            manufacturer: get_cm_property_string(devinst, CM_DRP_MFG).ok(),
            driver_version,
            driver_date,
            driver_path,
        };

        Ok(driver_info)
    }
} // Expand Windows indirect strings (e.g., @oem16.inf,%pid_082d%;HD Pro Webcam C920)
// Uses SHLoadIndirectString to resolve, with fallback to text after semicolon
fn expand_indirect_string(s: &str) -> Option<String> {
    if !s.starts_with('@') {
        return Some(s.to_string());
    }

    unsafe {
        // Use SHLoadIndirectString to resolve the indirect string
        let s_wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buffer = vec![0u16; STRING_BUFFER_SIZE];

        let hr = SHLoadIndirectString(PCWSTR(s_wide.as_ptr()), &mut buffer, None);

        if hr.is_ok() {
            let str_len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            if str_len > 0 {
                return Some(String::from_utf16_lossy(&buffer[..str_len]));
            }
        }

        // Fallback: extract text after semicolon if present
        if let Some(semicolon_pos) = s.rfind(';') {
            return Some(s[semicolon_pos + 1..].to_string());
        }

        Some(s.to_string())
    }
}

/// Helper to safely call Windows APIs that use UTF-16 buffers with size negotiation
/// Handles the common pattern: call with initial buffer, resize if needed, retry
///
/// # Arguments
/// * `initial_size` - Initial buffer size in u16 units (typically STRING_BUFFER_SIZE)
/// * `call_fn` - Closure that performs the FFI call: `|buffer, len| -> result_code`
/// * `success_check` - Closure that checks if result indicates success
fn call_with_utf16_buffer_generic<F, R>(
    initial_size: usize,
    mut call_fn: F,
    is_buffer_small: impl Fn(&R) -> bool,
    is_success: impl Fn(&R) -> bool,
) -> Result<String>
where
    F: FnMut(&mut Vec<u16>, &mut u32) -> R,
{
    let mut buffer = vec![0u16; initial_size];
    let mut buffer_len = (buffer.len() * 2) as u32;

    // First attempt
    let mut result = call_fn(&mut buffer, &mut buffer_len);

    // Resize and retry if buffer was too small
    if is_buffer_small(&result) {
        let new_len = (buffer_len as usize) / 2;
        buffer.resize(new_len, 0);
        result = call_fn(&mut buffer, &mut buffer_len);
    }

    if !is_success(&result) {
        anyhow::bail!("Windows API call failed");
    }

    let str_len = (buffer_len as usize / 2).saturating_sub(1);
    if str_len > 0 {
        Ok(String::from_utf16_lossy(&buffer[..str_len]).to_string())
    } else {
        anyhow::bail!("Empty property value")
    }
}

unsafe fn get_cm_property_string(devinst: u32, property: u32) -> Result<String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::*;

    unsafe {
        let mut reg_data_type: u32 = 0;
        let value = call_with_utf16_buffer_generic(
            STRING_BUFFER_SIZE,
            |buffer, buffer_len| {
                CM_Get_DevNode_Registry_PropertyW(
                    devinst,
                    property,
                    Some(&mut reg_data_type),
                    Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
                    buffer_len,
                    0,
                )
            },
            |cr: &CONFIGRET| cr.0 == CR_BUFFER_SMALL.0,
            |cr: &CONFIGRET| cr.0 == CR_SUCCESS.0,
        )?;

        Ok(expand_indirect_string(&value).unwrap_or(value))
    }
}
/// RAII guard for registry key cleanup
struct RegKeyGuard(windows::Win32::System::Registry::HKEY);

impl Drop for RegKeyGuard {
    fn drop(&mut self) {
        unsafe {
            use windows::Win32::System::Registry::*;
            let _ = RegCloseKey(self.0);
        }
    }
}

unsafe fn get_driver_info_from_cm(
    devinst: u32,
) -> Result<(Option<String>, Option<String>, Option<String>)> {
    unsafe {
        use windows::Win32::System::Registry::*;

        let mut hkey = HKEY::default();
        let cr = CM_Open_DevNode_Key(
            devinst,
            KEY_READ.0,
            0,
            RegDisposition_OpenExisting,
            &mut hkey,
            CM_REGISTRY_SOFTWARE,
        );

        if cr != CR_SUCCESS {
            return Ok((None, None, None));
        }

        // Use RAII guard to ensure registry key is closed even on early return or panic
        let _guard = RegKeyGuard(hkey);

        let driver_version = read_reg_value(&hkey, "DriverVersion").ok();
        let driver_date = read_reg_value(&hkey, "DriverDate").ok();

        // Get INF path which points to the driver installation file
        let driver_path = read_reg_value(&hkey, "InfPath").ok();

        Ok((driver_version, driver_date, driver_path))
    }
}

unsafe fn read_reg_value(
    hkey: &windows::Win32::System::Registry::HKEY,
    value_name: &str,
) -> Result<String> {
    use windows::Win32::System::Registry::*;

    unsafe {
        let value_name_wide: Vec<u16> = value_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        call_with_utf16_buffer_generic(
            STRING_BUFFER_SIZE,
            |buffer, buffer_len| {
                RegQueryValueExW(
                    *hkey,
                    PCWSTR(value_name_wide.as_ptr()),
                    None,
                    None,
                    Some(buffer.as_mut_ptr() as *mut u8),
                    Some(buffer_len),
                )
            },
            |err: &WIN32_ERROR| err.0 == ERROR_MORE_DATA.0,
            |err: &WIN32_ERROR| err.0 == ERROR_SUCCESS.0,
        )
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
            let _is_auto_mode = if get_value(&iface, prop_id, &mut value, &mut flags_val).is_ok() {
                trace!(property = %name, value, flags = flags_val, "Get successful");
                // If auto mode is enabled, record it (value formatting done on demand in UI)
                match property_type {
                    PropertyType::VideoProcAmp => {
                        (caps & VideoProcAmp_Flags_Auto.0 != 0)
                            && (flags_val & VideoProcAmp_Flags_Auto.0 != 0)
                    }
                    PropertyType::CameraControl => {
                        (caps & CameraControl_Flags_Auto.0 != 0)
                            && (flags_val & CameraControl_Flags_Auto.0 != 0)
                    }
                }
            } else {
                false
            };

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

unsafe fn get_video_proc_amp_properties(moniker: &IMoniker) -> Result<Vec<PropertyInfo>> {
    get_properties(
        moniker,
        |f| f.cast().context("Failed to get IAMVideoProcAmp interface"),
        &[
            // ...existing code...
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
            iface.GetRange(prop_id, min, max, step, default, caps)
        },
        |iface: &IAMVideoProcAmp, prop_id, value, flags| unsafe {
            iface.Get(prop_id, value, flags)
        },
        PropertyType::VideoProcAmp,
    )
}

unsafe fn get_camera_control_properties(moniker: &IMoniker) -> Result<Vec<PropertyInfo>> {
    get_properties(
        moniker,
        |f| f.cast().context("Failed to get IAMCameraControl interface"),
        &[
            // ...existing code...
            CameraControlProperty::Exposure,
            CameraControlProperty::Focus,
            CameraControlProperty::Pan,
            CameraControlProperty::Tilt,
            CameraControlProperty::Roll,
            CameraControlProperty::Zoom,
            CameraControlProperty::Iris,
        ],
        |iface: &IAMCameraControl, prop_id, min, max, step, default, caps| unsafe {
            iface.GetRange(prop_id, min, max, step, default, caps)
        },
        |iface: &IAMCameraControl, prop_id, value, flags| unsafe {
            iface.Get(prop_id, value, flags)
        },
        PropertyType::CameraControl,
    )
}

/// Find a device moniker by its DirectShow device path
/// Used by set_property functions to locate the device for property modification
unsafe fn find_device_by_path(target_path: &str) -> Result<IMoniker> {
    unsafe {
        let dev_enum: ICreateDevEnum =
            CoCreateInstance(&CLSID_SYSTEM_DEVICE_ENUM, None, CLSCTX_INPROC_SERVER)?;

        let mut enum_moniker: Option<IEnumMoniker> = None;
        dev_enum.CreateClassEnumerator(&CLSID_VIDEO_INPUT_DEVICE_CATEGORY, &mut enum_moniker, 0)?;

        let Some(enum_moniker) = enum_moniker else {
            anyhow::bail!("No video devices found");
        };

        loop {
            let mut monikers: [Option<IMoniker>; 1] = [None];
            let mut fetched = 0u32;

            let hr = enum_moniker.Next(&mut monikers, Some(&mut fetched));

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
}

/// Set a VideoProcAmp property
pub fn set_video_proc_amp_property(
    device: &DeviceInfo,
    property: VideoProcAmpProperty,
    value: i32,
    auto: bool,
) -> Result<()> {
    let _com = unsafe { ComGuard::new()? };

    let target_path = device
        .device_path
        .as_ref()
        .context("Device path not available")?;

    let mon = unsafe { find_device_by_path(target_path)? };
    let filter: IBaseFilter =
        unsafe { mon.BindToObject(None, None) }.context("Failed to bind to device filter")?;
    let video_proc_amp: IAMVideoProcAmp = filter
        .cast()
        .context("Failed to get VideoProcAmp interface")?;

    let flags = if auto {
        VideoProcAmp_Flags_Auto.0
    } else {
        VideoProcAmp_Flags_Manual.0
    };
    unsafe { video_proc_amp.Set(property.into(), value, flags) }.with_context(|| {
        format!(
            "Failed to set VideoProcAmp property {} to value {}",
            property, value
        )
    })?;

    Ok(())
}

/// Set a CameraControl property
pub fn set_camera_control_property(
    device: &DeviceInfo,
    property: CameraControlProperty,
    value: i32,
    auto: bool,
) -> Result<()> {
    let _com = unsafe { ComGuard::new()? };

    let target_path = device
        .device_path
        .as_ref()
        .context("Device path not available")?;

    let mon = unsafe { find_device_by_path(target_path)? };
    let filter: IBaseFilter =
        unsafe { mon.BindToObject(None, None) }.context("Failed to bind to device filter")?;
    let camera_control: IAMCameraControl = filter
        .cast()
        .context("Failed to get CameraControl interface")?;

    let flags = if auto {
        CameraControl_Flags_Auto.0
    } else {
        CameraControl_Flags_Manual.0
    };
    unsafe { camera_control.Set(property.into(), value, flags) }.with_context(|| {
        format!(
            "Failed to set CameraControl property {} to value {}",
            property, value
        )
    })?;

    Ok(())
}

/// Set a property by name on a device
/// High-level function that:
/// - Parses the value string (handles Auto, 50Hz, On/Off, etc.)
/// - Validates the property exists and value is within safe ranges
/// - Determines if it's a VideoProcAmp or CameraControl property
/// - Calls the appropriate low-level setter function
pub fn set_property(device: &DeviceInfo, property_name: &str, value_str: &str) -> Result<()> {
    // Sanitize property name - only allow alphanumeric characters
    if !property_name.chars().all(|c| c.is_alphanumeric()) {
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
            "Value {} for property '{}' is outside the safe range {}-{} (device reports: min={}, max={})",
            numeric_value,
            property_name,
            min,
            max,
            min,
            max
        );
    }

    match property_type {
        PropertyType::VideoProcAmp => {
            let prop_enum: VideoProcAmpProperty = property_name
                .parse()
                .map_err(|_| anyhow::anyhow!("Unknown VideoProcAmp property: {}", property_name))?;
            set_video_proc_amp_property(device, prop_enum, numeric_value, auto_mode)
        }
        PropertyType::CameraControl => {
            let prop_enum: CameraControlProperty = property_name.parse().map_err(|_| {
                anyhow::anyhow!("Unknown CameraControl property: {}", property_name)
            })?;
            set_camera_control_property(device, prop_enum, numeric_value, auto_mode)
        }
    }
}

/// Generate enum mapping for special properties
fn get_enum_mapping(property_name: &str, min: i32, max: i32) -> Option<Vec<(i32, String)>> {
    match property_name {
        "PowerlineFrequency" => {
            let mut mapping = vec![];
            if min <= 0 && max >= 0 {
                mapping.push((0, "Disabled".to_string()));
            }
            if min <= 1 && max >= 1 {
                mapping.push((1, "50Hz".to_string()));
            }
            if min <= 2 && max >= 2 {
                mapping.push((2, "60Hz".to_string()));
            }
            if min <= 3 && max >= 3 {
                mapping.push((3, "Auto".to_string()));
            }
            Some(mapping)
        }
        "ColorEnable" | "BacklightCompensation" => {
            let mut mapping = vec![];
            if min <= 0 && max >= 0 {
                mapping.push((0, "Off".to_string()));
            }
            if min <= 1 && max >= 1 {
                mapping.push((1, "On".to_string()));
            }
            Some(mapping)
        }
        _ => None,
    }
}
