#[cfg(target_os = "macos")]
use core_foundation::base::TCFType;
#[cfg(target_os = "macos")]
use core_foundation::string::{CFString, CFStringRef};
#[cfg(target_os = "macos")]
use log::{debug, error, info, warn};
#[cfg(target_os = "macos")]
use std::ffi::c_void;
#[cfg(target_os = "macos")]
use tokio::sync::mpsc as tokio_mpsc;
#[cfg(target_os = "macos")]
use tokio::time::{interval, Duration};

#[cfg(target_os = "macos")]
#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        inObjectID: u32,
        inAddress: *const AudioObjectPropertyAddress,
        inQualifierDataSize: u32,
        inQualifierData: *const c_void,
        ioDataSize: *mut u32,
        outData: *mut c_void,
    ) -> i32;
    
    fn AudioHardwareGetProperty(
        inPropertyID: u32,
        ioPropertyDataSize: *mut u32,
        outPropertyData: *mut c_void,
    ) -> i32;
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct AudioObjectPropertyAddress {
    mSelector: u32,
    mScope: u32,
    mElement: u32,
}

#[cfg(target_os = "macos")]
// CoreAudio property selectors (four-char codes)
// Four-character codes in CoreAudio: 'dout' = 0x646f7574 when interpreted as big-endian
// But on macOS, four-char codes are stored in the native format
// Try both formats to see which works
const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE_BE: u32 = u32::from_be_bytes([b'd', b'o', b'u', b't']); // 0x646f7574
const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE_LE: u32 = u32::from_le_bytes([b'd', b'o', b'u', b't']); // 0x74756f64
// Use the big-endian version (standard CoreAudio format)
const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE: u32 = K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE_BE;
#[cfg(target_os = "macos")]
// 'dnam' = kAudioDevicePropertyDeviceNameCFString  
const K_AUDIO_DEVICE_PROPERTY_DEVICE_NAME_CF_STRING: u32 = u32::from_be_bytes([b'd', b'n', b'a', b'm']);

#[cfg(target_os = "macos")]
// CoreAudio scopes - these are numeric values, not four-char codes
const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = 0; // kAudioObjectPropertyScopeGlobal
#[cfg(target_os = "macos")]
const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0; // kAudioObjectPropertyElementMain

#[cfg(target_os = "macos")]
// System object ID - kAudioObjectSystemObject
const K_AUDIO_OBJECT_SYSTEM_OBJECT: u32 = 1;

#[cfg(target_os = "macos")]
// CoreAudio error codes
const K_AUDIO_HARDWARE_NO_ERROR: i32 = 0;

#[cfg(target_os = "macos")]
/// Get the name of the default output device using system_profiler
fn get_default_output_device_name() -> Option<String> {
    use std::process::Command;
    
    // Use system_profiler to get the default output device name
    // This is more reliable than CoreAudio FFI
    let output = Command::new("system_profiler")
        .arg("SPAudioDataType")
        .arg("-json")
        .output()
        .ok()?;
    
    if !output.status.success() {
        debug!("system_profiler failed");
        return None;
    }
    
    // Parse JSON to find default output device
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    
    // Navigate through the JSON structure to find default output device
    if let Some(items) = json.get("SPAudioDataType")?.as_array() {
        for item in items {
            if let Some(devices) = item.get("_items")?.as_array() {
                for device in devices {
                    // Look for the default output device
                    // It has "coreaudio_default_audio_output_device" : "spaudio_yes"
                    if let Some(default_output) = device.get("coreaudio_default_audio_output_device") {
                        if default_output.as_str() == Some("spaudio_yes") {
                            if let Some(name) = device.get("_name")?.as_str() {
                                return Some(name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    
    None
}


#[cfg(target_os = "macos")]
/// Start monitoring for audio device changes on macOS using polling
pub fn start_device_monitor(
    event_tx: tokio_mpsc::UnboundedSender<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting macOS audio device monitor (polling mode, using system_profiler)");

    // Get initial device name - if this fails, we'll just start monitoring anyway
    let mut last_device_name = match std::panic::catch_unwind(|| get_default_output_device_name()) {
        Ok(Some(name)) => {
            info!("Initial audio device: {name}");
            Some(name)
        }
        Ok(None) => {
            warn!("Could not get initial audio device name, will monitor anyway");
            None
        }
        Err(_) => {
            warn!("Panic while getting initial audio device name, will monitor anyway");
            None
        }
    };

    // Spawn a task that polls for device changes every 500ms
    tokio::spawn(async move {
        let mut poll_interval = interval(Duration::from_millis(500));
        // Skip the first tick to avoid immediate check
        poll_interval.tick().await;

        let mut poll_count = 0u32;
        loop {
            poll_interval.tick().await;
            poll_count += 1;

            // Safely get current device name
            let current_device_name = match std::panic::catch_unwind(|| get_default_output_device_name()) {
                Ok(name) => name,
                Err(_) => {
                    warn!("Panic while getting audio device name, continuing...");
                    continue;
                }
            };

            // Log periodically for debugging
            if poll_count % 20 == 0 {
                debug!("Polling audio device (count: {}), current: {:?}, last: {:?}", 
                       poll_count, current_device_name, last_device_name);
            }

            // Check if device changed
            if current_device_name != last_device_name {
                info!("Audio output device changed from {:?} to {:?}", last_device_name, current_device_name);
                
                let device_name = current_device_name.clone().unwrap_or_default();
                info!("Sending device change event with device name: '{}'", 
                      if device_name.is_empty() { "default" } else { &device_name });
                if let Err(e) = event_tx.send(device_name) {
                    error!("Failed to send device change event: {e}");
                    break;
                }

                last_device_name = current_device_name;
            }
        }
    });

    Ok(())
}

#[cfg(not(target_os = "macos"))]
/// No-op for non-macOS platforms
pub fn start_device_monitor(
    _event_tx: tokio_mpsc::UnboundedSender<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
