//! HID interfészek felsorolása (SetupAPI), alapadataik, és időkorlátos olvasás/írás.

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Devices::Properties::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::IO::*;

#[derive(Clone)]
pub struct HidInterface {
    /// Nullával lezárt eszközútvonal.
    pub path: Vec<u16>,
    pub vid: u16,
    pub pid: u16,
    pub product: String,
    pub serial: String,
    pub usage_page: u16,
    pub usage: u16,
    pub input_len: u16,
}

impl HidInterface {
    /// Bluetooth Classic HID (HIDP szolgáltatás UUID az útvonalban).
    pub fn is_bluetooth(&self) -> bool {
        String::from_utf16_lossy(&self.path).to_ascii_lowercase().contains("00001124-0000-1000-8000-00805f9b34fb")
    }
}

/// Az összes jelenlévő HID interfész. Egy olvasási körön belül (2 mp) a providerek ugyanazt a
/// listát kapják, hogy a felsorolás ne fusson le többször.
pub fn interfaces() -> Vec<HidInterface> {
    static CACHE: Mutex<Option<(Instant, Vec<HidInterface>)>> = Mutex::new(None);
    let Ok(mut cache) = CACHE.lock() else { return Vec::new() };
    if let Some((at, list)) = cache.as_ref() {
        if at.elapsed() < Duration::from_secs(2) {
            return list.clone();
        }
    }
    let list: Vec<HidInterface> = unsafe { paths().iter().filter_map(|(p, desc)| info(p, desc.as_deref())).collect() };
    *cache = Some((Instant::now(), list.clone()));
    list
}

/// Sorozatszámot csak ezektől kérünk (az azonosítóhoz kell); a lekérdezés egyes eszközöknél
/// (pl. energiatakarékos USB) másodpercekig is eltarthat.
const SERIAL_VIDS: [u16; 3] = [0x054C, 0x3554, 0x046D];

/// DEVPKEY_Device_BusReportedDeviceDesc: a termék neve, ahogy az eszköz csatlakozáskor megadta.
/// A Windows eltárolja, így nem kell érte az eszközhöz fordulni.
const BUS_DESC: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x540b947e_8b40_45bc_a8a2_6a0b894cbda2), pid: 4 };

/// A HID csomóponton általában nincs ilyen, a szülőn (az USB interfészen) igen.
unsafe fn bus_desc(devinst: u32) -> Option<String> {
    let mut parent = 0u32;
    let parent = (CM_Get_Parent(&mut parent, devinst, 0) == CR_SUCCESS).then_some(parent);
    [Some(devinst), parent].into_iter().flatten().find_map(|node| {
        let mut ty = DEVPROPTYPE(0);
        let mut buf = [0u8; 512];
        let mut size = buf.len() as u32;
        let cr = CM_Get_DevNode_PropertyW(node, &BUS_DESC, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0);
        if cr != CR_SUCCESS {
            return None;
        }
        let w: Vec<u16> = buf[..size as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(wide_string(&w)).filter(|s| !s.is_empty())
    })
}

/// Útvonalak, mellettük a Windows által eltárolt terméknév (ha van).
unsafe fn paths() -> Vec<(Vec<u16>, Option<String>)> {
    let guid = HidD_GetHidGuid();
    let Ok(set) = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for i in 0.. {
        let mut ifd = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        if SetupDiEnumDeviceInterfaces(set, None, &guid, i, &mut ifd).is_err() {
            break;
        }
        let mut need = 0u32;
        let _ = SetupDiGetDeviceInterfaceDetailW(set, &ifd, None, 0, Some(&mut need), None);
        // u64-es puffer, hogy a struktúra igazítása rendben legyen
        let mut buf = vec![0u64; (need as usize).div_ceil(8)];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        let mut dev = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        if SetupDiGetDeviceInterfaceDetailW(set, &ifd, Some(detail), need, None, Some(&mut dev)).is_err() {
            continue;
        }
        let ptr = (*detail).DevicePath.as_ptr();
        let len = (0..).take_while(|&k| *ptr.add(k) != 0).count();
        paths.push((std::slice::from_raw_parts(ptr, len + 1).to_vec(), bus_desc(dev.DevInst)));
    }
    let _ = SetupDiDestroyDeviceInfoList(set);
    paths
}

fn wide_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 0 hozzáféréssel a rendszer által foglalt billentyűzet/egér interfészek is megnyithatók.
unsafe fn info(path: &[u16], desc: Option<&str>) -> Option<HidInterface> {
    let h = CreateFileW(
        PCWSTR(path.as_ptr()),
        0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        None,
    )
    .ok()?;
    let mut attrs = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    let ok = HidD_GetAttributes(h, &mut attrs).as_bool();
    let product = match desc {
        Some(d) => d.to_string(),
        None => {
            let mut prod = [0u16; 128];
            let _ = HidD_GetProductString(h, prod.as_mut_ptr() as *mut c_void, (prod.len() * 2) as u32);
            wide_string(&prod)
        }
    };
    let mut serial = [0u16; 128];
    if SERIAL_VIDS.contains(&attrs.VendorID) {
        let _ = HidD_GetSerialNumberString(h, serial.as_mut_ptr() as *mut c_void, (serial.len() * 2) as u32);
    }
    let mut caps = HIDP_CAPS::default();
    let mut pp = PHIDP_PREPARSED_DATA::default();
    if HidD_GetPreparsedData(h, &mut pp).as_bool() {
        let _ = HidP_GetCaps(pp, &mut caps);
        let _ = HidD_FreePreparsedData(pp);
    }
    let _ = CloseHandle(h);
    ok.then(|| HidInterface {
        path: path.to_vec(),
        vid: attrs.VendorID,
        pid: attrs.ProductID,
        product,
        serial: wide_string(&serial),
        usage_page: caps.UsagePage,
        usage: caps.Usage,
        input_len: caps.InputReportByteLength,
    })
}

/// Írás-olvasásra megnyitott HID interfész, időkorlátos (overlapped) olvasással.
pub struct Device {
    h: HANDLE,
    ev: HANDLE,
}

impl Device {
    pub fn open(path: &[u16]) -> Option<Device> {
        unsafe {
            let h = CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
            .ok()?;
            match CreateEventW(None, true, false, PCWSTR::null()) {
                Ok(ev) => Some(Device { h, ev }),
                Err(_) => {
                    let _ = CloseHandle(h);
                    None
                }
            }
        }
    }

    /// Egy kimenő report (az első bájt a report ID); false hiba vagy időtúllépés esetén.
    pub fn write(&self, data: &[u8], timeout_ms: u32) -> bool {
        unsafe {
            let _ = ResetEvent(self.ev);
            let mut ov = OVERLAPPED { hEvent: self.ev, ..Default::default() };
            if let Err(e) = WriteFile(self.h, Some(data), None, Some(&mut ov)) {
                if e.code() != ERROR_IO_PENDING.to_hresult() {
                    return false;
                }
            }
            let mut n = 0u32;
            if WaitForSingleObject(self.ev, timeout_ms) != WAIT_OBJECT_0 {
                let _ = CancelIoEx(self.h, Some(&ov));
                let _ = GetOverlappedResult(self.h, &ov, &mut n, true);
                return false;
            }
            GetOverlappedResult(self.h, &ov, &mut n, false).is_ok()
        }
    }

    /// Egy bejövő report (az első bájt a report ID), vagy None időtúllépéskor.
    pub fn read(&self, len: usize, timeout_ms: u32) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; len];
        unsafe {
            let _ = ResetEvent(self.ev);
            let mut ov = OVERLAPPED { hEvent: self.ev, ..Default::default() };
            if let Err(e) = ReadFile(self.h, Some(&mut buf), None, Some(&mut ov)) {
                if e.code() != ERROR_IO_PENDING.to_hresult() {
                    return None;
                }
            }
            let mut n = 0u32;
            if WaitForSingleObject(self.ev, timeout_ms) != WAIT_OBJECT_0 {
                let _ = CancelIoEx(self.h, Some(&ov));
                let _ = GetOverlappedResult(self.h, &ov, &mut n, true);
                return None;
            }
            GetOverlappedResult(self.h, &ov, &mut n, false).ok()?;
            buf.truncate(n as usize);
        }
        Some(buf)
    }

    /// Feature report lekérése (`len` bájt a report ID-vel együtt).
    pub fn get_feature(&self, id: u8, len: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; len];
        buf[0] = id;
        unsafe { HidD_GetFeature(self.h, buf.as_mut_ptr() as *mut c_void, len as u32).as_bool().then_some(buf) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Interfészenkénti lekérdezési idő (`cargo test hid_timing -- --ignored --nocapture`).
    #[test]
    #[ignore = "a csatlakoztatott hardvertől függ"]
    fn hid_timing() {
        let t = Instant::now();
        let all = unsafe { paths() };
        println!("felsorolás: {:.1?}", t.elapsed());
        for (p, desc) in all {
            let t = Instant::now();
            let i = unsafe { info(&p, desc.as_deref()) };
            let name = i.map(|i| format!("{:04X}:{:04X} {} 0x{:04X}", i.vid, i.pid, i.product, i.usage_page)).unwrap_or_default();
            println!("{:>8.1?} {name}  [{}]", t.elapsed(), if desc.is_some() { "tárolt név" } else { "eszköztől" });
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.ev);
            let _ = CloseHandle(self.h);
        }
    }
}
