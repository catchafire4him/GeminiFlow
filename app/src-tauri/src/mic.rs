//! Reads and sets the Windows input level for the microphone.
//!
//! Deliberately the real device level rather than gain applied after capture.
//! Post-capture gain multiplies noise along with speech; the endpoint level is
//! the control Windows itself exposes, so changing it here means one setting
//! rather than two that interact confusingly.
//!
//! This is a system-wide setting: turning it up here turns it up for every
//! application, exactly as the Windows Sound panel would.

use anyhow::{anyhow, Result};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;

/// COM has to be initialised on every thread that touches it. Tauri commands
/// run on a pool, so this cannot be done once at startup.
fn ensure_com() {
    unsafe {
        // Already-initialised is a success for our purposes; the returned
        // HRESULT is deliberately ignored rather than unwrapped.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// The endpoint volume interface for the configured microphone.
///
/// Matches by friendly name when Settings names a device, so the slider
/// controls the microphone actually being recorded rather than whatever
/// Windows currently calls the default.
fn endpoint(preferred: Option<&str>) -> Result<IAudioEndpointVolume> {
    ensure_com();

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| anyhow!("could not reach the audio devices: {e}"))?;

        if let Some(name) = preferred {
            if let Ok(collection) =
                enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            {
                for i in 0..collection.GetCount().unwrap_or(0) {
                    let Ok(device) = collection.Item(i) else { continue };
                    let Ok(store) = device.OpenPropertyStore(
                        windows::Win32::System::Com::STGM_READ,
                    ) else {
                        continue;
                    };
                    let Ok(value) = store.GetValue(&PKEY_Device_FriendlyName) else {
                        continue;
                    };
                    let Ok(pwstr) = PropVariantToStringAlloc(&value) else { continue };
                    let friendly = pwstr.to_string().unwrap_or_default();

                    // cpal reports the same friendly name, but truncated on
                    // some devices, so this is a containment test rather than
                    // an equality one.
                    if friendly == name || friendly.contains(name) || name.contains(&friendly) {
                        return device
                            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                            .map_err(|e| anyhow!("could not open the level control: {e}"));
                    }
                }
            }
        }

        let device = enumerator
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .map_err(|e| anyhow!("no default microphone: {e}"))?;
        device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .map_err(|e| anyhow!("could not open the level control: {e}"))
    }
}

/// The default playback device's volume control.
///
/// Here only so a spare dial has something worth doing while nothing is
/// recording. Always the system default rather than a configured device:
/// there is no setting for which speakers to use, and inventing one to
/// serve a dial would be the tail wagging the dog.
fn output_endpoint() -> Result<IAudioEndpointVolume> {
    ensure_com();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| anyhow!("could not reach the audio devices: {e}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| anyhow!("no default speakers: {e}"))?;
        device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .map_err(|e| anyhow!("could not open the volume control: {e}"))
    }
}

pub fn output_volume() -> Result<i64> {
    let endpoint = output_endpoint()?;
    let scalar = unsafe { endpoint.GetMasterVolumeLevelScalar() }
        .map_err(|e| anyhow!("could not read the volume: {e}"))?;
    Ok((scalar * 100.0).round() as i64)
}

pub fn set_output_volume(percent: i64) -> Result<()> {
    let endpoint = output_endpoint()?;
    let scalar = percent.clamp(0, 100) as f32 / 100.0;
    unsafe { endpoint.SetMasterVolumeLevelScalar(scalar, std::ptr::null()) }
        .map_err(|e| anyhow!("could not set the volume: {e}"))
}

/// Current level, 0-100.
pub fn volume(preferred: Option<&str>) -> Result<i64> {
    let endpoint = endpoint(preferred)?;
    let scalar = unsafe { endpoint.GetMasterVolumeLevelScalar() }
        .map_err(|e| anyhow!("could not read the microphone level: {e}"))?;
    Ok((scalar * 100.0).round() as i64)
}

pub fn set_volume(preferred: Option<&str>, percent: i64) -> Result<()> {
    let endpoint = endpoint(preferred)?;
    let scalar = percent.clamp(0, 100) as f32 / 100.0;
    unsafe { endpoint.SetMasterVolumeLevelScalar(scalar, std::ptr::null()) }
        .map_err(|e| anyhow!("could not set the microphone level: {e}"))?;
    crate::logln!("[mic] input level set to {percent}%");
    Ok(())
}

/// Whether the microphone is muted in Windows.
///
/// Worth surfacing separately: a muted microphone at 100% level looks fine in
/// a slider and records pure silence.
pub fn is_muted(preferred: Option<&str>) -> Result<bool> {
    let endpoint = endpoint(preferred)?;
    let muted = unsafe { endpoint.GetMute() }
        .map_err(|e| anyhow!("could not read the mute state: {e}"))?;
    Ok(muted.as_bool())
}

pub fn set_muted(preferred: Option<&str>, muted: bool) -> Result<()> {
    let endpoint = endpoint(preferred)?;
    unsafe { endpoint.SetMute(muted, std::ptr::null()) }
        .map_err(|e| anyhow!("could not change the mute state: {e}"))?;
    crate::logln!("[mic] {}", if muted { "muted" } else { "unmuted" });
    Ok(())
}
