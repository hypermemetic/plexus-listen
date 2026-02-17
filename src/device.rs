//! USB device discovery (rusb) and audio device matching (cpal)

use cpal::traits::{DeviceTrait, HostTrait};

use crate::types::{AudioDevice, DeviceKind};

/// DJI MIC MINI USB vendor/product IDs
const DJI_VENDOR_ID: u16 = 0x2CA3;
const DJI_MIC_MINI_PRODUCT_ID: u16 = 0x0042;

/// Known DJI MIC device name substrings
const DJI_DEVICE_NAMES: &[&str] = &["DJI", "dji"];

/// USB device information
#[derive(Debug, Clone)]
pub struct UsbDeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub name: String,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
}

/// Read a USB string descriptor using the first available language
fn read_usb_string(
    handle: &rusb::DeviceHandle<rusb::GlobalContext>,
    desc: &rusb::DeviceDescriptor,
    reader: impl FnOnce(&rusb::DeviceHandle<rusb::GlobalContext>, rusb::Language, &rusb::DeviceDescriptor, std::time::Duration) -> rusb::Result<String>,
) -> Option<String> {
    let timeout = std::time::Duration::from_millis(200);
    let languages = handle.read_languages(timeout).ok()?;
    let lang = *languages.first()?;
    reader(handle, lang, desc, timeout).ok()
}

/// Find the DJI MIC MINI USB device via rusb
pub fn find_dji_usb_device() -> Option<UsbDeviceInfo> {
    let devices = match rusb::devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("Failed to enumerate USB devices: {}", e);
            return None;
        }
    };

    for device in devices.iter() {
        let desc = match device.device_descriptor() {
            Ok(d) => d,
            Err(_) => continue,
        };

        if desc.vendor_id() == DJI_VENDOR_ID {
            let handle = match device.open() {
                Ok(h) => h,
                Err(_) => {
                    return Some(UsbDeviceInfo {
                        vendor_id: desc.vendor_id(),
                        product_id: desc.product_id(),
                        name: format!("DJI Device {:04x}:{:04x}", desc.vendor_id(), desc.product_id()),
                        serial: None,
                        manufacturer: None,
                    });
                }
            };

            let name = read_usb_string(&handle, &desc, |h, l, d, t| h.read_product_string(l, d, t))
                .unwrap_or_else(|| format!("DJI Device {:04x}:{:04x}", desc.vendor_id(), desc.product_id()));
            let serial = read_usb_string(&handle, &desc, |h, l, d, t| h.read_serial_number_string(l, d, t));
            let manufacturer = read_usb_string(&handle, &desc, |h, l, d, t| h.read_manufacturer_string(l, d, t));

            return Some(UsbDeviceInfo {
                vendor_id: desc.vendor_id(),
                product_id: desc.product_id(),
                name,
                serial,
                manufacturer,
            });
        }
    }

    // Fallback: scan for any DJI product ID match
    for device in devices.iter() {
        let desc = match device.device_descriptor() {
            Ok(d) => d,
            Err(_) => continue,
        };

        if desc.product_id() == DJI_MIC_MINI_PRODUCT_ID {
            let name = device.open().ok()
                .and_then(|handle| read_usb_string(&handle, &desc, |h, l, d, t| h.read_product_string(l, d, t)))
                .unwrap_or_else(|| "DJI MIC MINI".to_string());

            return Some(UsbDeviceInfo {
                vendor_id: desc.vendor_id(),
                product_id: desc.product_id(),
                name,
                serial: None,
                manufacturer: None,
            });
        }
    }

    None
}

/// Check if a cpal device name looks like a DJI mic
fn is_dji_device_name(name: &str) -> bool {
    DJI_DEVICE_NAMES.iter().any(|pattern| name.contains(pattern))
}

/// List all audio input devices, marking which ones are DJI
pub fn list_audio_input_devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let mut devices = Vec::new();

    let input_devices = match host.input_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("Failed to enumerate audio input devices: {}", e);
            return devices;
        }
    };

    for device in input_devices {
        let name = device.name().unwrap_or_else(|_| "Unknown".to_string());
        let config = device.default_input_config();

        let (channels, sample_rate) = match config {
            Ok(c) => (c.channels(), c.sample_rate().0),
            Err(_) => (0, 0),
        };

        devices.push(AudioDevice {
            name: name.clone(),
            channels,
            sample_rate,
            is_dji: is_dji_device_name(&name),
            kind: DeviceKind::Hardware,
        });
    }

    // Append capturable applications on macOS (silently skip on permission denial)
    #[cfg(target_os = "macos")]
    {
        if let Ok(apps) = crate::sck::list_capturable_apps() {
            for app in apps {
                devices.push(AudioDevice {
                    name: app.name.clone(),
                    channels: 2,
                    sample_rate: 48000,
                    is_dji: false,
                    kind: DeviceKind::Application,
                });
            }
        }
    }

    devices
}

/// Find the cpal input device matching the DJI MIC MINI
pub fn find_dji_audio_device() -> Option<cpal::Device> {
    find_audio_device(None)
}

/// Find an audio input device by name substring, or auto-detect DJI if name is None
pub fn find_audio_device(name: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    let input_devices = host.input_devices().ok()?;

    match name {
        Some(query) => {
            for device in input_devices {
                if let Ok(dev_name) = device.name() {
                    if dev_name.to_lowercase().contains(&query.to_lowercase()) {
                        return Some(device);
                    }
                }
            }
            None
        }
        None => {
            for device in input_devices {
                if let Ok(dev_name) = device.name() {
                    if is_dji_device_name(&dev_name) {
                        return Some(device);
                    }
                }
            }
            None
        }
    }
}

/// Get the preferred stream config for a capture device (48kHz, 2ch preferred)
pub fn preferred_stream_config(device: &cpal::Device) -> Option<cpal::StreamConfig> {
    let supported = device.supported_input_configs().ok()?;

    // Prefer 48kHz 2ch config
    for config in supported {
        if config.channels() == 2
            && config.min_sample_rate().0 <= 48000
            && config.max_sample_rate().0 >= 48000
        {
            return Some(cpal::StreamConfig {
                channels: 2,
                sample_rate: cpal::SampleRate(48000),
                buffer_size: cpal::BufferSize::Default,
            });
        }
    }

    // Fallback to default
    device
        .default_input_config()
        .ok()
        .map(|c| c.config())
}

/// Find a capturable application by case-insensitive name substring.
#[cfg(target_os = "macos")]
pub fn find_app_source(query: &str) -> Option<crate::sck::AppSource> {
    let apps = crate::sck::list_capturable_apps().ok()?;
    let query_lower = query.to_lowercase();
    apps.into_iter().find(|app| app.name.to_lowercase().contains(&query_lower))
}
