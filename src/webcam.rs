/// Webcam and DirectShow interaction module
///
/// This module handles all DirectShow COM interactions for webcam device enumeration,
/// property querying and setting, and device information retrieval. It provides a domain
/// layer abstraction over Windows DirectShow APIs with type-safe property enums and
/// value formatting.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use strum::Display;
use tracing::{debug, instrument, trace};
use windows::{
    Win32::Devices::DeviceAndDriverInstallation::*, Win32::Foundation::*,
    Win32::Media::DirectShow::*, Win32::System::Com::StructuredStorage::*, Win32::System::Com::*,
    Win32::UI::Shell::*, core::*,
};

// String buffer size for Windows API calls (registry, property bags, etc.)
const STRING_BUFFER_SIZE: usize = 512;

// DirectShow property capability flags (used for Auto vs Manual mode)
const CAPABILITY_AUTO: i32 = 0x1;
const CAPABILITY_MANUAL: i32 = 0x2;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
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

impl std::str::FromStr for VideoProcAmpProperty {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Brightness" => Ok(VideoProcAmpProperty::Brightness),
            "Contrast" => Ok(VideoProcAmpProperty::Contrast),
            "Hue" => Ok(VideoProcAmpProperty::Hue),
            "Saturation" => Ok(VideoProcAmpProperty::Saturation),
            "Sharpness" => Ok(VideoProcAmpProperty::Sharpness),
            "Gamma" => Ok(VideoProcAmpProperty::Gamma),
            "ColorEnable" => Ok(VideoProcAmpProperty::ColorEnable),
            "WhiteBalance" => Ok(VideoProcAmpProperty::WhiteBalance),
            "BacklightCompensation" => Ok(VideoProcAmpProperty::BacklightCompensation),
            "Gain" => Ok(VideoProcAmpProperty::Gain),
            "DigitalMultiplier" => Ok(VideoProcAmpProperty::DigitalMultiplier),
            "DigitalMultiplierLimit" => Ok(VideoProcAmpProperty::DigitalMultiplierLimit),
            "WhiteBalanceComponent" => Ok(VideoProcAmpProperty::WhiteBalanceComponent),
            "PowerlineFrequency" => Ok(VideoProcAmpProperty::PowerlineFrequency),
            _ => Err(anyhow::anyhow!("Unknown VideoProcAmp property: {}", s)),
        }
    }
}

/// CameraControl property IDs from DirectShow (ksmedia.h)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
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

impl std::str::FromStr for CameraControlProperty {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Pan" => Ok(CameraControlProperty::Pan),
            "Tilt" => Ok(CameraControlProperty::Tilt),
            "Roll" => Ok(CameraControlProperty::Roll),
            "Zoom" => Ok(CameraControlProperty::Zoom),
            "Exposure" => Ok(CameraControlProperty::Exposure),
            "Iris" => Ok(CameraControlProperty::Iris),
            "Focus" => Ok(CameraControlProperty::Focus),
            _ => Err(anyhow::anyhow!("Unknown CameraControl property: {}", s)),
        }
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
    pub supported_values: String,
    pub default: String,
    pub current: Option<String>,
    pub capabilities: Option<String>,
    pub property_type: PropertyType,
}

/// Format a property value into a human-readable label based on the property name
fn format_property_value(property_name: &str, value: i32) -> String {
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

/// Generate supported values string based on property name and min/max values
/// Returns either a range ("0-255") or enumerated values ("50Hz, 60Hz")
/// Appends "Auto" if the property supports automatic mode
fn get_supported_values(property_name: &str, min: i32, max: i32, caps: i32) -> String {
    let base_values = match property_name {
        "PowerlineFrequency" => {
            let mut values = vec![];
            if min <= 0 && max >= 0 {
                values.push("Disabled");
            }
            if min <= 1 && max >= 1 {
                values.push("50Hz");
            }
            if min <= 2 && max >= 2 {
                values.push("60Hz");
            }
            if min <= 3 && max >= 3 {
                values.push("Auto");
            }
            values.join(", ")
        }
        "ColorEnable" | "BacklightCompensation" => {
            let mut values = vec![];
            if min <= 0 && max >= 0 {
                values.push("Off");
            }
            if min <= 1 && max >= 1 {
                values.push("On");
            }
            values.join(", ")
        }
        _ => format!("{}-{}", min, max),
    };

    // Append Auto if the property supports automatic mode
    if caps & CAPABILITY_AUTO != 0 {
        format!("{}, Auto", base_values)
    } else {
        base_values
    }
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
unsafe fn get_device_name(moniker: &IMoniker) -> Result<String> {
    debug!("Reading FriendlyName from property bag");
    unsafe {
        let result = get_property_string(moniker, "FriendlyName");
        if let Ok(ref name) = result {
            debug!(friendly_name = %name, "Device name obtained");
        } else {
            trace!("Failed to read FriendlyName");
        }
        result
    }
}

#[instrument(skip(moniker))]
unsafe fn get_device_path(moniker: &IMoniker) -> Result<String> {
    debug!("Reading DevicePath from property bag");
    unsafe {
        let result = get_property_string(moniker, "DevicePath");
        if let Ok(ref path) = result {
            trace!(device_path = %path, "Device path obtained");
        } else {
            trace!("Failed to read DevicePath");
        }
        result
    }
}

#[instrument(skip(moniker))]
unsafe fn get_property_string(moniker: &IMoniker, prop_name: &str) -> Result<String> {
    trace!(property_name = %prop_name, "Binding moniker to property bag");
    // Use HSTRING for property name
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
    let device_instance_id = parse_device_instance_id(device_path).with_context(|| {
        format!(
            "Failed to parse device instance ID from path: {}",
            device_path
        )
    })?;

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
            anyhow::bail!("Failed to locate device node for driver info");
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

unsafe fn get_cm_property_string(devinst: u32, property: u32) -> Result<String> {
    unsafe {
        let mut buffer = vec![0u16; STRING_BUFFER_SIZE];
        let mut buffer_len = (buffer.len() * 2) as u32;
        let mut reg_data_type: u32 = 0;

        let cr = CM_Get_DevNode_Registry_PropertyW(
            devinst,
            property,
            Some(&mut reg_data_type),
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            &mut buffer_len as *mut u32,
            0,
        );

        if cr != CR_SUCCESS {
            anyhow::bail!("Failed to get CM property");
        }

        let str_len = (buffer_len as usize / 2).saturating_sub(1);
        if str_len > 0 {
            let value = String::from_utf16_lossy(&buffer[..str_len]);
            return Ok(expand_indirect_string(&value).unwrap_or(value));
        }

        anyhow::bail!("Empty property value")
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
    unsafe {
        use windows::Win32::Foundation::*;
        use windows::Win32::System::Registry::*;

        let value_name_wide: Vec<u16> = value_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut buffer = vec![0u16; STRING_BUFFER_SIZE];
        let mut buffer_size = (buffer.len() * 2) as u32;

        let result = RegQueryValueExW(
            *hkey,
            PCWSTR(value_name_wide.as_ptr()),
            None,
            None,
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut buffer_size),
        );

        if result != ERROR_SUCCESS {
            anyhow::bail!("Failed to read registry value '{}'", value_name);
        }

        let str_len = (buffer_size as usize / 2).saturating_sub(1);
        let value = String::from_utf16_lossy(&buffer[..str_len]);

        Ok(value)
    }
}

#[instrument(skip(moniker))]
unsafe fn get_video_proc_amp_properties(moniker: &IMoniker) -> Result<Vec<PropertyInfo>> {
    debug!("Binding moniker to IBaseFilter");
    let filter: IBaseFilter = unsafe { moniker.BindToObject(None, None) }
        .context("Failed to bind to IBaseFilter for VideoProcAmp")?;
    trace!("Casting IBaseFilter to IAMVideoProcAmp");
    let video_proc_amp: IAMVideoProcAmp = filter
        .cast()
        .context("Failed to get IAMVideoProcAmp interface")?;
    debug!("IAMVideoProcAmp interface obtained");

    let properties = [
        // Basic image controls
        VideoProcAmpProperty::Brightness,
        VideoProcAmpProperty::Contrast,
        VideoProcAmpProperty::Saturation,
        // Color controls
        VideoProcAmpProperty::Hue,
        VideoProcAmpProperty::WhiteBalance,
        VideoProcAmpProperty::WhiteBalanceComponent,
        VideoProcAmpProperty::ColorEnable,
        VideoProcAmpProperty::Gamma,
        // Advanced controls
        VideoProcAmpProperty::Sharpness,
        VideoProcAmpProperty::BacklightCompensation,
        VideoProcAmpProperty::Gain,
        VideoProcAmpProperty::PowerlineFrequency,
        VideoProcAmpProperty::DigitalMultiplier,
        VideoProcAmpProperty::DigitalMultiplierLimit,
    ];

    let mut capabilities = Vec::new();

    trace!(
        property_count = properties.len(),
        "Enumerating VideoProcAmp properties"
    );
    for property in properties {
        let prop_id: i32 = property.into();
        let name = property.to_string();
        let mut min = 0;
        let mut max = 0;
        let mut step = 0;
        let mut default = 0;
        let mut caps = 0;

        if unsafe {
            video_proc_amp.GetRange(
                prop_id,
                &mut min,
                &mut max,
                &mut step,
                &mut default,
                &mut caps,
            )
        }
        .is_ok()
        {
            trace!(property = %name, min, max, step, default, caps, "GetRange successful");
            let mut value = 0;
            let mut flags_val = 0;
            let current_formatted =
                if unsafe { video_proc_amp.Get(prop_id, &mut value, &mut flags_val) }.is_ok() {
                    trace!(property = %name, value, flags = flags_val, "Get successful");
                    // If auto mode is enabled, return "Auto" as the current value
                    if (caps & CAPABILITY_AUTO != 0) && (flags_val & CAPABILITY_AUTO != 0) {
                        Some("Auto".to_string())
                    } else {
                        Some(format_property_value(&name, value))
                    }
                } else {
                    None
                };

            capabilities.push(PropertyInfo {
                name: name.to_string(),
                supported_values: get_supported_values(&name, min, max, caps),
                default: format_property_value(&name, default),
                current: current_formatted,
                capabilities: format_capabilities(caps),
                property_type: PropertyType::VideoProcAmp,
            });
        } else {
            trace!(property = %name, "GetRange failed - property not supported");
        }
    }

    debug!(
        property_count = capabilities.len(),
        "VideoProcAmp property enumeration complete"
    );
    Ok(capabilities)
}

#[instrument(skip(moniker))]
unsafe fn get_camera_control_properties(moniker: &IMoniker) -> Result<Vec<PropertyInfo>> {
    debug!("Binding moniker to IBaseFilter");
    let filter: IBaseFilter = unsafe { moniker.BindToObject(None, None) }
        .context("Failed to bind to IBaseFilter for CameraControl")?;
    trace!("Casting IBaseFilter to IAMCameraControl");
    let camera_control: IAMCameraControl = filter
        .cast()
        .context("Failed to get IAMCameraControl interface")?;
    debug!("IAMCameraControl interface obtained");

    let properties = [
        // Primary image controls
        CameraControlProperty::Exposure,
        CameraControlProperty::Focus,
        // Mechanical positioning
        CameraControlProperty::Pan,
        CameraControlProperty::Tilt,
        CameraControlProperty::Roll,
        CameraControlProperty::Zoom,
        // Aperture
        CameraControlProperty::Iris,
    ];

    let mut capabilities = Vec::new();

    trace!(
        property_count = properties.len(),
        "Enumerating CameraControl properties"
    );
    for property in properties {
        let prop_id: i32 = property.into();
        let name = property.to_string();
        let mut min = 0;
        let mut max = 0;
        let mut step = 0;
        let mut default = 0;
        let mut caps = 0;

        if unsafe {
            camera_control.GetRange(
                prop_id,
                &mut min,
                &mut max,
                &mut step,
                &mut default,
                &mut caps,
            )
        }
        .is_ok()
        {
            trace!(property = %name, min, max, step, default, caps, "GetRange successful");
            let mut value = 0;
            let mut flags_val = 0;
            let current_formatted =
                if unsafe { camera_control.Get(prop_id, &mut value, &mut flags_val) }.is_ok() {
                    trace!(property = %name, value, flags = flags_val, "Get successful");
                    // If auto mode is enabled, return "Auto" as the current value
                    if (caps & CAPABILITY_AUTO != 0) && (flags_val & CAPABILITY_AUTO != 0) {
                        Some("Auto".to_string())
                    } else {
                        Some(format_property_value(&name, value))
                    }
                } else {
                    None
                };

            capabilities.push(PropertyInfo {
                name: name.to_string(),
                supported_values: get_supported_values(&name, min, max, caps),
                default: format_property_value(&name, default),
                current: current_formatted,
                capabilities: format_capabilities(caps),
                property_type: PropertyType::CameraControl,
            });
        } else {
            trace!(property = %name, "GetRange failed - property not supported");
        }
    }

    debug!(
        property_count = capabilities.len(),
        "CameraControl property enumeration complete"
    );
    Ok(capabilities)
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
        CAPABILITY_AUTO
    } else {
        CAPABILITY_MANUAL
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
        CAPABILITY_AUTO
    } else {
        CAPABILITY_MANUAL
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
    if !auto_mode {
        // Parse the supported_values to extract min/max range
        if let Some((min, max)) = parse_value_range(&prop_info.supported_values)
            && (numeric_value < min || numeric_value > max)
        {
            anyhow::bail!(
                "Value {} for property '{}' is outside the safe range {} (device reports: {})",
                numeric_value,
                property_name,
                prop_info.supported_values,
                prop_info.supported_values
            );
        }
    }

    match property_type {
        PropertyType::VideoProcAmp => {
            let prop_enum: VideoProcAmpProperty = property_name.parse()?;
            set_video_proc_amp_property(device, prop_enum, numeric_value, auto_mode)
        }
        PropertyType::CameraControl => {
            let prop_enum: CameraControlProperty = property_name.parse()?;
            set_camera_control_property(device, prop_enum, numeric_value, auto_mode)
        }
    }
}

/// Parse the supported values string to extract min/max range
/// Returns None if the format is not a simple range (e.g., for enumerated values)
fn parse_value_range(supported_values: &str) -> Option<(i32, i32)> {
    // Check if it's a range format like "0-255" or "0-255, Auto"
    let range_part = supported_values.split(',').next()?.trim();

    let parts: Vec<&str> = range_part.split('-').collect();
    if parts.len() == 2 {
        let min = parts[0].trim().parse::<i32>().ok()?;
        let max = parts[1].trim().parse::<i32>().ok()?;
        Some((min, max))
    } else {
        None
    }
}
