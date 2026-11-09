//! HID interfészek felsorolása (SetupAPI), alapadataik, és időkorlátos olvasás/írás.

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::IO::*;

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

/// Az összes jelenlévő HID interfész.
pub fn interfaces() -> Vec<HidInterface> {
    unsafe { paths().iter().filter_map(|p| info(p)).collect() }
}

unsafe fn paths() -> Vec<Vec<u16>> {
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
        if SetupDiGetDeviceInterfaceDetailW(set, &ifd, Some(detail), need, None, None).is_err() {
            continue;
        }
        let ptr = (*detail).DevicePath.as_ptr();
        let len = (0..).take_while(|&k| *ptr.add(k) != 0).count();
        paths.push(std::slice::from_raw_parts(ptr, len + 1).to_vec());
    }
    let _ = SetupDiDestroyDeviceInfoList(set);
    paths
}

fn wide_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 0 hozzáféréssel a rendszer által foglalt billentyűzet/egér interfészek is megnyithatók.
unsafe fn info(path: &[u16]) -> Option<HidInterface> {
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
    let mut prod = [0u16; 128];
    let _ = HidD_GetProductString(h, prod.as_mut_ptr() as *mut c_void, (prod.len() * 2) as u32);
    let mut serial = [0u16; 128];
    let _ = HidD_GetSerialNumberString(h, serial.as_mut_ptr() as *mut c_void, (serial.len() * 2) as u32);
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
        product: wide_string(&prod),
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

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.ev);
            let _ = CloseHandle(self.h);
        }
    }
}
