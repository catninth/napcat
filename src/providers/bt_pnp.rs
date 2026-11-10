//! Bluetooth eszközök akkuszintje a Windows saját PnP propertyjéből
//! ({104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2). Ezt használja a Windows Beállítások akkukijelzése
//! is: Bluetooth egerek, billentyűzetek, fejhallgatók, ha a Windows ismeri a szintjüket.
//! A property nem hivatalosan dokumentált; ha egy Windows frissítés megszünteti, a ble_gatt a
//! tartalék út.

use std::collections::HashMap;
use std::mem::size_of;
use std::time::Duration;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Properties::*;

use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const BATTERY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5), pid: 2 };
const NAME: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac), pid: 10 };
const CONTAINER_ID: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c), pid: 2 };
/// Bluetooth Class of Device (Classic BT): a fő- és alosztályból jön az eszköztípus.
const CLASS_OF_DEVICE: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x2bd67d8b_8beb_48d5_87e0_6cda3428040a), pid: 10 };
/// Csatlakoztatva-e (DEVPROP_TYPE_BOOLEAN) a Bluetooth devnode-okon. A párosított eszközök
/// devnode-jai kikapcsolva is "jelen vannak", és az akku property a legutóbbi szintet őrzi;
/// ebből látszik, hogy most valóban kapcsolódik-e.
const IS_CONNECTED: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x83DA6326_97A6_4088_9453_A1923F573B29), pid: 15 };

unsafe fn property(set: HDEVINFO, dev: &SP_DEVINFO_DATA, key: &DEVPROPKEY, buf: &mut [u8]) -> Option<usize> {
    let mut ty = DEVPROPTYPE(0);
    let mut size = 0u32;
    SetupDiGetDevicePropertyW(set, dev, key, &mut ty, Some(buf), Some(&mut size), 0).ok()?;
    Some(size as usize)
}

unsafe fn string_property(set: HDEVINFO, dev: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let mut buf = [0u8; 512];
    let n = property(set, dev, key, &mut buf)?;
    let w: Vec<u16> = buf[..n].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    Some(String::from_utf16_lossy(&w))
}

/// Eszköztípus a Class of Device-ból (perifériánál a billentyűzet/mutató bitek, audiónál
/// fejhallgató), különben a névből.
fn kind_of(cod: Option<u32>, name: &str) -> DeviceKind {
    if let Some(cod) = cod {
        let (major, minor) = ((cod >> 8) & 0x1F, (cod >> 2) & 0x3F);
        match major {
            0x05 if minor & 0x10 != 0 => return DeviceKind::Keyboard,
            0x05 if minor & 0x20 != 0 => return DeviceKind::Mouse,
            0x05 if minor & 0x0F == 0x01 || minor & 0x0F == 0x02 => return DeviceKind::Controller,
            0x04 => return DeviceKind::Headset,
            _ => {}
        }
    }
    let n = name.to_lowercase();
    if ["keyboard", "billentyű", "keys"].iter().any(|k| n.contains(k)) {
        DeviceKind::Keyboard
    } else if ["mouse", "egér", "mx master", "trackball"].iter().any(|k| n.contains(k)) {
        DeviceKind::Mouse
    } else if ["controller", "gamepad", "dualsense", "xbox", "joy-con", "pro controller"].iter().any(|k| n.contains(k)) {
        DeviceKind::Controller
    } else if ["headset", "headphone", "buds", "airpods", "fejhallgató", "earphone", "wh-", "wf-"].iter().any(|k| n.contains(k)) {
        DeviceKind::Headset
    } else {
        DeviceKind::Other
    }
}

/// Egy akkus Bluetooth eszköz a devnode-jai alapján.
struct Found {
    id: String,
    name: String,
    kind: DeviceKind,
    level: u8,
}

/// A container csatlakozás-jelzői: ha bármelyik devnode-ja igazat mond, csatlakoztatva van; ha
/// van jelző, de mind hamis, nincs; jelző nélkül (régebbi Windows, más illesztő) csatlakoztatottnak
/// vesszük, ahogy eddig.
fn connected(flags: Option<&Vec<bool>>) -> bool {
    flags.map_or(true, |f| f.iter().any(|&c| c))
}

/// Minden jelenlévő eszköz, amelyen van akku property, és hogy most csatlakoztatva van-e.
fn scan_devices() -> Vec<(Found, bool)> {
    let mut found: Vec<Found> = Vec::new();
    // Container -> a devnode-jain talált csatlakozás-jelzők.
    let mut flags: HashMap<String, Vec<bool>> = HashMap::new();
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES | DIGCF_PRESENT) else { return Vec::new() };
        for i in 0.. {
            let mut dev = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, i, &mut dev).is_err() {
                break;
            }
            // Egy fizikai eszköz több devnode-on is hordozhatja: a container azonosító egyesíti őket.
            let mut container = [0u8; 16];
            let container_id = match property(set, &dev, &CONTAINER_ID, &mut container) {
                Some(16) => Some(format!("bt-{:032x}", u128::from_le_bytes(container))),
                _ => None,
            };
            let mut flag = [0u8; 1];
            if let (Some(c), Some(1)) = (&container_id, property(set, &dev, &IS_CONNECTED, &mut flag)) {
                flags.entry(c.clone()).or_default().push(flag[0] != 0);
            }
            let mut level = [0u8; 4];
            if property(set, &dev, &BATTERY, &mut level).is_none() {
                continue;
            }
            let id = container_id.unwrap_or_else(|| format!("bt-{i}"));
            if found.iter().any(|d| d.id == id) {
                continue;
            }
            let name = string_property(set, &dev, &NAME).unwrap_or_else(|| crate::i18n::t("Bluetooth eszköz", "Bluetooth device").into());
            let mut cod = [0u8; 4];
            let cod = property(set, &dev, &CLASS_OF_DEVICE, &mut cod).filter(|&n| n == 4).map(|_| u32::from_le_bytes(cod));
            found.push(Found { id, kind: kind_of(cod, &name), name, level: level[0].min(100) });
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    found.into_iter().map(|f| { let c = connected(flags.get(&f.id)); (f, c) }).collect()
}

#[derive(Default)]
pub struct BtPnp {
    seen: HashMap<String, (String, DeviceKind)>,
    connected: usize,
}

impl Provider for BtPnp {
    fn scan(&mut self) -> Vec<DeviceState> {
        let mut out = Vec::new();
        for (Found { id, name, kind, level }, connected) in scan_devices() {
            self.seen.insert(id.clone(), (name.clone(), kind));
            // Párosított, de most nem kapcsolódó eszköz: a tárolt szint régi, nem mutatjuk.
            if !connected {
                out.push(DeviceState::disconnected(&id, &name, kind));
                continue;
            }
            out.push(DeviceState {
                id,
                name,
                kind,
                level: Some(level),
                level_approx: false,
                // A property csak a szintet adja; a töltést a Windows itt nem jelzi.
                charge: ChargeState::Unknown,
                connected: true,
                hw: None, estimate: None,
            });
        }
        // A párosított eszközök kikapcsolva is a listában vannak: a lekérdezés addig fut, amíg
        // van akár csak párosított eszköz, hogy a visszakapcsolást is észrevegyük.
        self.connected = out.len();
        for (id, (name, kind)) in &self.seen {
            if !out.iter().any(|d| &d.id == id) {
                out.push(DeviceState::disconnected(id, name, *kind));
            }
        }
        out
    }

    /// A property változásáról nincs olcsó értesítés, ezért 5 percenként újraolvassuk,
    /// de csak amíg van ilyen eszköz.
    fn poll_interval(&self) -> Option<Duration> {
        (self.connected > 0).then_some(Duration::from_secs(300))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_class_of_device_and_name() {
        assert_eq!(kind_of(Some(0x002540), "x"), DeviceKind::Keyboard); // periféria, billentyűzet
        assert_eq!(kind_of(Some(0x002580), "x"), DeviceKind::Mouse); // periféria, mutató
        assert_eq!(kind_of(Some(0x002508), "x"), DeviceKind::Controller); // periféria, gamepad
        assert_eq!(kind_of(Some(0x240404), "x"), DeviceKind::Headset); // audio/video
        assert_eq!(kind_of(None, "Logitech G435"), DeviceKind::Other);
        assert_eq!(kind_of(None, "Sony WH-1000XM4"), DeviceKind::Headset);
        assert_eq!(kind_of(None, "R75 Keyboard"), DeviceKind::Keyboard);
    }

    #[test]
    fn connection_flags() {
        assert!(connected(None)); // nincs jelző: a régi viselkedés
        assert!(connected(Some(&vec![false, true])));
        assert!(!connected(Some(&vec![false, false])));
    }

    /// A gépen most látható Bluetooth akku propertyk (`cargo test bt_present -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn bt_present() {
        let t = std::time::Instant::now();
        for d in BtPnp::default().scan() {
            println!("{}", d.tooltip());
        }
        println!("scan(): {:?}", t.elapsed());
    }
}
