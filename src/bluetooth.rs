//! Which Bluetooth headset the sound goes to: the default output's physical device (its
//! "container"), when one of that device's nodes is a Bluetooth one, with the name and the
//! battery Windows knows for it. Blocking WinRT calls; run off the window's thread.

use windows::{
    core::*,
    Devices::Enumeration::{
        DeviceInformation,
        Pnp::{PnpObject, PnpObjectType},
    },
    Foundation::IPropertyValue,
    Media::Devices::{AudioDeviceRole, MediaDevice},
};
use windows_collections::IIterable;

/// DEVPKEY_Bluetooth_Battery: percent left, as a byte, on the node that reports it.
const BATTERY: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
const NAME: &str = "System.ItemNameDisplay";

#[derive(Debug)]
pub struct Device {
    pub name: String,
    pub battery: Option<u8>,
}

/// The default output, when it is a Bluetooth device.
pub fn output() -> Result<Option<Device>> {
    let id = MediaDevice::GetDefaultAudioRenderId(AudioDeviceRole::Default)?;
    let properties = IIterable::<HSTRING>::from(vec![HSTRING::from("System.Devices.ContainerId")]);
    let endpoint = DeviceInformation::CreateFromIdAsyncAdditionalProperties(&id, &properties)?.join()?;
    let container = endpoint.Properties()?.Lookup(h!("System.Devices.ContainerId"))?.cast::<IPropertyValue>()?.GetGuid()?;
    in_container(container, &endpoint.Name()?.to_string())
}

/// The Bluetooth device whose nodes share `container`; `fallback` names it when none does.
fn in_container(container: GUID, fallback: &str) -> Result<Option<Device>> {
    let wanted = IIterable::<HSTRING>::from(vec![HSTRING::from(NAME), HSTRING::from(BATTERY)]);
    let filter = HSTRING::from(format!("System.Devices.ContainerId:=\"{{{container:?}}}\""));
    let mut nodes: Vec<(String, Option<String>, Option<u8>)> = Vec::new();
    for node in &PnpObject::FindAllAsyncAqsFilter(PnpObjectType::Device, &wanted, &filter)?.join()? {
        let values = node.Properties()?;
        let value = |key: &str| values.Lookup(&HSTRING::from(key)).ok().and_then(|v| v.cast::<IPropertyValue>().ok());
        let name = value(NAME).and_then(|v| v.GetString().ok()).map(|s| s.to_string());
        let battery = value(BATTERY).and_then(|v| v.GetUInt8().ok());
        nodes.push((node.Id()?.to_string(), name, battery));
    }
    Ok(device(&nodes, fallback))
}

/// From a container's nodes (instance id, name, battery): a Bluetooth device's name and
/// battery; `fallback` names it when no node does.
fn device(nodes: &[(String, Option<String>, Option<u8>)], fallback: &str) -> Option<Device> {
    let bluetooth = |id: &str| ["BTHENUM\\", "BTHLE\\", "BTHLEDEVICE\\", "BTHHFENUM\\"].iter().any(|bus| id.to_uppercase().starts_with(bus));
    if !nodes.iter().any(|(id, ..)| bluetooth(id)) {
        return None;
    }
    // The device itself ("BTHENUM\DEV_…", "BTHLE\DEV_…") carries the name people gave it.
    let own = |(id, name, _): &(String, Option<String>, Option<u8>)| id.to_uppercase().contains("\\DEV_").then(|| name.clone()).flatten();
    let name = nodes.iter().find_map(own).unwrap_or_else(|| fallback.to_string());
    let battery = nodes.iter().filter_map(|(.., battery)| *battery).max();
    Some(Device { name, battery })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_headset_in_its_container() {
        let node = |id: &str, name: Option<&str>, battery| (id.to_string(), name.map(String::from), battery);
        let airpods = [
            node("BTHENUM\\{0000111E-0000-1000-8000-00805F9B34FB}_VID&0001004C\\7&3B0F&0&EC46_C00000000", Some("AirPods Hands-Free AG"), Some(80)),
            node("BTHENUM\\DEV_EC46545CDE65\\7&3B0FBDF8&0&BLUETOOTHDEVICE_EC46545CDE65", Some("AirPods"), None),
            node("SWD\\MMDEVAPI\\{0.0.0.00000000}.{6A7B}", Some("Headphones"), None),
        ];
        let found = device(&airpods, "Headphones (AirPods)").unwrap();
        assert_eq!((found.name.as_str(), found.battery), ("AirPods", Some(80)));
        // Speakers on a USB or HD Audio device are no headset.
        let speakers = [node("HDAUDIO\\FUNC_01&VEN_10EC", Some("Realtek"), None), node("SWD\\MMDEVAPI\\{0.0.0.00000000}.{1}", Some("Speakers"), None)];
        assert!(device(&speakers, "Speakers").is_none());
        // No node with its own name: the endpoint's.
        let bare = [node("BTHLEDEVICE\\{0000180F}_DEV_VID&01", None, None)];
        assert_eq!(device(&bare, "Headset").unwrap().name, "Headset");
    }

    /// This machine's devices: `cargo test -- --ignored --nocapture`. ISLAND_CONTAINER may name
    /// a paired headset's container to look at, connected or not.
    #[test]
    #[ignore]
    fn reads_this_machines_devices() {
        unsafe { windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED).unwrap() };
        println!("default output: {:?}", output());
        if let Ok(container) = std::env::var("ISLAND_CONTAINER") {
            println!("{container}: {:?}", in_container(GUID::try_from(container.as_str()).unwrap(), "?"));
        }
    }
}
