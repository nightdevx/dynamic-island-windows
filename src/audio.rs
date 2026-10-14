//! System volume: the audio service calls back when the default output device's volume or
//! mute changes, and when another device becomes the default. Nothing polls; each callback
//! posts a message to the window and returns.

use windows::{
    core::*,
    Win32::{
        Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM},
        Media::Audio::{Endpoints::*, *},
        System::Com::{CoCreateInstance, CLSCTX_ALL},
        UI::WindowsAndMessaging::PostMessageW,
    },
};

/// Volume 0..100 in WPARAM, muted in LPARAM.
#[implement(IAudioEndpointVolumeCallback)]
struct OnVolume {
    window: isize,
    message: u32,
}

impl IAudioEndpointVolumeCallback_Impl for OnVolume_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        let Some(data) = (unsafe { data.as_ref() }) else { return Ok(()) };
        let level = (data.fMasterVolume * 100.0).round() as usize;
        unsafe { PostMessageW(Some(HWND(self.window as _)), self.message, WPARAM(level), LPARAM(data.bMuted.0 as isize)) }
    }
}

#[implement(IMMNotificationClient)]
struct OnDevice {
    window: isize,
    message: u32,
}

impl IMMNotificationClient_Impl for OnDevice_Impl {
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eMultimedia {
            unsafe { PostMessageW(Some(HWND(self.window as _)), self.message, WPARAM(0), LPARAM(0))? };
        }
        Ok(())
    }

    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

pub struct Volume {
    devices: IMMDeviceEnumerator,
    endpoint: Option<IAudioEndpointVolume>,
    on_volume: IAudioEndpointVolumeCallback,
    on_device: IMMNotificationClient,
}

impl Volume {
    /// Posts `volume_message` on each volume change and `device_message` when the default
    /// output changes; the window then calls `follow_default`.
    pub fn new(hwnd: HWND, volume_message: u32, device_message: u32) -> Result<Self> {
        let window = hwnd.0 as isize;
        let devices: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let on_device: IMMNotificationClient = OnDevice { window, message: device_message }.into();
        unsafe { devices.RegisterEndpointNotificationCallback(&on_device)? };
        let on_volume = OnVolume { window, message: volume_message }.into();
        Ok(Self { devices, endpoint: None, on_volume, on_device })
    }

    /// Listens to the current default output device and returns its volume and mute.
    pub fn follow_default(&mut self) -> Result<(u32, bool)> {
        if let Some(old) = self.endpoint.take() {
            unsafe {
                let _ = old.UnregisterControlChangeNotify(&self.on_volume);
            }
        }
        unsafe {
            let device = self.devices.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
            let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            endpoint.RegisterControlChangeNotify(&self.on_volume)?;
            let level = (endpoint.GetMasterVolumeLevelScalar()? * 100.0).round() as u32;
            let muted = endpoint.GetMute()?.as_bool();
            self.endpoint = Some(endpoint);
            Ok((level, muted))
        }
    }
}

/// Whether a process with this (lowercase) exe name plays for the media session `app_id`
/// (lowercase): "brave" plays from brave.exe, "spotify.exe" from spotify.exe.
pub fn plays_for(app_id: &str, exe: &str) -> bool {
    let stem = exe.trim_end_matches(".exe");
    stem.len() >= 3 && app_id.contains(stem)
}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe {
            if let Some(endpoint) = &self.endpoint {
                let _ = endpoint.UnregisterControlChangeNotify(&self.on_volume);
            }
            let _ = self.devices.UnregisterEndpointNotificationCallback(&self.on_device);
        }
    }
}
