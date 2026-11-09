//! HID interfészek felsorolása (SetupAPI) és alapadataik (0 jogosultsággal megnyitva).

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

pub struct HidInterface {
    pub vid: u16,
    pub pid: u16,
    pub product: String,
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
    let _ = CloseHandle(h);
    let end = prod.iter().position(|&c| c == 0).unwrap_or(prod.len());
    ok.then(|| HidInterface { vid: attrs.VendorID, pid: attrs.ProductID, product: String::from_utf16_lossy(&prod[..end]) })
}
