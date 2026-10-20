//! Enumeration of active audio capture endpoints.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{GUID, PWSTR, Result};

/// Endpoint property holding the underlying device instance path, e.g.
/// `{1}.USB\VID_0D8C&PID_0012&MI_00\...`, or `{1}.BTHHFENUM\...` for a Bluetooth headset.
const PKEY_ENDPOINT_DEVICE_PATH: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xb3f8fa53_0004_438e_9003_51a46e139bfc),
    pid: 2,
};
const MAX_LABEL_CHARS: usize = 12;
const NOISE_WORDS: [&str; 5] = ["audio", "device", "technology", "for", "high"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable endpoint id string (used to reopen the device).
    pub id: String,
    pub friendly_name: String,
    /// Bluetooth endpoints switch the headset to the low-quality hands-free profile
    /// while a capture stream is open, so they are skipped by default.
    pub bluetooth: bool,
    /// Underlying device interface path (empty if unavailable); shown by `--list`.
    pub device_path: String,
}

pub fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: COM is initialized on the calling thread by the caller.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

/// Lists active capture devices, in the order Windows reports them.
pub fn active_capture_devices(enumerator: &IMMDeviceEnumerator) -> Result<Vec<DeviceInfo>> {
    // SAFETY: plain COM calls on valid interface pointers.
    unsafe {
        let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let count = collection.GetCount()?;
        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = collection.Item(index)?;
            let path = string_property(&device, &PKEY_ENDPOINT_DEVICE_PATH).unwrap_or_default();
            devices.push(DeviceInfo {
                id: device_id(&device)?,
                friendly_name: string_property(&device, &PKEY_Device_FriendlyName)
                    .unwrap_or_else(|_| "Mic".to_owned()),
                device_path: path.clone(),
                bluetooth: is_bluetooth_path(&path),
            });
        }
        Ok(devices)
    }
}

fn device_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: GetId returns a CoTaskMem-allocated string we own and free.
    unsafe { take_co_string(device.GetId()?) }
}

fn string_property(device: &IMMDevice, key: &PROPERTYKEY) -> Result<String> {
    // SAFETY: read-only property store access.
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        let value = store.GetValue(key)?;
        Ok(value.to_string())
    }
}

/// True for endpoint device paths of Bluetooth enumerators (classic, hands-free, LE audio).
fn is_bluetooth_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    ["bthenum", "bthhfenum", "bthleenum", "bthle"]
        .iter()
        .any(|bus| lower.contains(bus))
}

unsafe fn take_co_string(ptr: PWSTR) -> Result<String> {
    // SAFETY: caller guarantees `ptr` is a valid CoTaskMem string.
    let text = unsafe { ptr.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(ptr.0 as *const _)) };
    Ok(text)
}

/// Builds a short label: the text inside the first parentheses (usually the hardware name),
/// stripped of trademark noise and truncated.
pub fn short_label(friendly_name: &str) -> String {
    let inner = friendly_name
        .split_once('(')
        .and_then(|(_, rest)| rest.rsplit_once(')').map(|(inside, _)| inside))
        .filter(|inside| !inside.trim().is_empty())
        .unwrap_or_else(|| friendly_name.split('(').next().unwrap_or(friendly_name));
    let cleaned = inner
        .replace("(R)", "")
        .replace("(TM)", "")
        .replace(['®', '™'], "");
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let meaningful: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| !NOISE_WORDS.contains(&word.to_lowercase().as_str()))
        .collect();
    let chosen = if meaningful.is_empty() {
        words
    } else {
        meaningful
    };
    truncate(&chosen.join(" "), MAX_LABEL_CHARS)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max_chars - 1).collect();
    short = short.trim_end().to_owned();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bluetooth_device_paths() {
        assert!(is_bluetooth_path(
            r"{2}.\\?\BTHHFENUM#{0000111e-0000-1000}_VID&0002"
        ));
        assert!(!is_bluetooth_path(r"{2}.\\?\usb#vid_0c76&pid_161f&mi_00"));
    }

    #[test]
    fn short_label_prefers_parenthesised_hardware_name() {
        assert_eq!(short_label("Microphone (Realtek(R) Audio)"), "Realtek");
        assert_eq!(short_label("Headset Microphone (USB Audio Device)"), "USB");
        assert_eq!(short_label("Mic"), "Mic");
        assert_eq!(short_label("Stereo Mix ()"), "Stereo Mix");
        assert_eq!(
            short_label("Microphone Array (Intel® Smart Sound Technology for Digital Microphones)"),
            "Intel Smart…"
        );
    }
}
