//! wincamcfg library - DirectShow camera property management.
//!
//! This library provides safe abstractions over Windows DirectShow APIs for
//! managing webcam properties. It supports both real DirectShow operations
//! and mockable implementations for testing.

pub mod webcam;

pub use webcam::{
    CameraControlProperty, DeviceInfo, DeviceListItem, PropertyInfo, PropertyType,
    VideoProcAmpProperty, enumerate_devices, format_property_value, list_devices,
    parse_property_value,
};

#[cfg(all(test, feature = "test-util"))]
mod tests {
    use crate::webcam::MockCamera;
    use crate::webcam::{
        CameraDevice, VideoProcAmpProperty, format_property_value, parse_property_value,
    };

    #[test]
    fn mock_camera_creation() {
        let camera = MockCamera::new(
            Some("Test Camera".to_string()),
            Some("TestDevice123".to_string()),
        );

        assert_eq!(camera.name, Some("Test Camera".to_string()));
        assert_eq!(camera.device_path, Some("TestDevice123".to_string()));
    }

    #[test]
    fn mock_camera_get_device_info() -> anyhow::Result<()> {
        let camera = MockCamera::new(
            Some("Test Camera".to_string()),
            Some("/dev/video0".to_string()),
        );

        let (name, path) = camera.get_device_info()?;
        assert_eq!(name, Some("Test Camera".to_string()));
        assert_eq!(path, Some("/dev/video0".to_string()));

        Ok(())
    }

    #[test]
    fn mock_camera_get_video_proc_amp_properties() -> anyhow::Result<()> {
        let camera = MockCamera::new(None, None);

        let props = camera.get_video_proc_amp_properties()?;
        assert!(!props.is_empty());

        let brightness = props
            .iter()
            .find(|p| p.name == "Brightness")
            .expect("Brightness property should exist");
        assert_eq!(brightness.min, Some(0));
        assert_eq!(brightness.max, Some(255));
        assert_eq!(brightness.current, Some(128));

        Ok(())
    }

    #[test]
    fn mock_camera_set_video_proc_amp_property() -> anyhow::Result<()> {
        let mut camera = MockCamera::new(None, None);

        camera.set_video_proc_amp_property(VideoProcAmpProperty::Brightness, 200, false)?;

        let props = camera.get_video_proc_amp_properties()?;
        let brightness = props
            .iter()
            .find(|p| p.name == "Brightness")
            .expect("Brightness property should exist");
        assert_eq!(brightness.current, Some(200));

        Ok(())
    }

    #[test]
    fn format_property_value_powerline_frequency() {
        assert_eq!(format_property_value("PowerlineFrequency", 0), "Disabled");
        assert_eq!(format_property_value("PowerlineFrequency", 1), "50Hz");
        assert_eq!(format_property_value("PowerlineFrequency", 2), "60Hz");
        assert_eq!(format_property_value("PowerlineFrequency", 3), "Auto");
    }

    #[test]
    fn parse_property_value_auto_mode() -> anyhow::Result<()> {
        let (_value, auto) = parse_property_value("PowerlineFrequency", "Auto")?;
        assert!(auto);
        Ok(())
    }

    #[test]
    fn parse_property_value_label() -> anyhow::Result<()> {
        let (value, auto) = parse_property_value("PowerlineFrequency", "50Hz")?;
        assert!(!auto);
        assert_eq!(value, 1);
        Ok(())
    }
}
