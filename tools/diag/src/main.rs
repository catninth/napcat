//! NapCat diagnosztika.
//!
//! 1. Minden eszköz, amin van Bluetooth akku property ({104EA319-…} 2) – ezt használja a
//!    Windows Beállítások is.
//! 2. Minden HID interfész: VID/PID, név, usage page, és a szabványos akku usage-ek
//!    (0x06/0x20 Battery Strength, 0x84 Power Device, 0x85 Battery System). Ha van ilyen,
//!    megpróbálja ki is olvasni. A gyártóspecifikus (0xFF00+) interfészek report ID-it is
//!    kiírja a későbbi visszafejtéshez.

#![allow(non_upper_case_globals)] // a windows crate HidP_* konstansai mintában

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Devices::Properties::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

const BT_BATTERY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5),
    pid: 2,
};
const NAME: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac),
    pid: 10,
};

mod compx;
mod io;
mod listen;
mod via;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("via") => unsafe { via::run() },
        Some("compx") => unsafe { compx::run() },
        Some("listen") => {
            let secs = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(15);
            unsafe { listen::run(secs) }
        }
        None => list(),
        Some(_) => println!("használat: napcat-diag [via | compx | listen [mp]]"),
    }
}

fn list() {
    println!("=== Bluetooth akku property ===");
    if let Err(e) = unsafe { bt_battery() } {
        println!("hiba: {e}");
    }
    println!();
    println!("=== HID interfészek ===");
    if let Err(e) = unsafe { hid_devices() } {
        println!("hiba: {e}");
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

unsafe fn dev_string_prop(set: HDEVINFO, dev: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<String> {
    let mut ty = DEVPROPTYPE(0);
    let mut buf = [0u8; 512];
    SetupDiGetDevicePropertyW(set, dev, key, &mut ty, Some(&mut buf), None, 0).ok()?;
    let w: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(wide_to_string(&w))
}

unsafe fn bt_battery() -> windows::core::Result<()> {
    let set = SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES | DIGCF_PRESENT)?;
    let mut found = 0;
    for i in 0.. {
        let mut dev = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        if SetupDiEnumDeviceInfo(set, i, &mut dev).is_err() {
            break;
        }
        let mut ty = DEVPROPTYPE(0);
        let mut buf = [0u8; 4];
        if SetupDiGetDevicePropertyW(set, &dev, &BT_BATTERY, &mut ty, Some(&mut buf), None, 0).is_err() {
            continue;
        }
        found += 1;
        let name = dev_string_prop(set, &dev, &NAME).unwrap_or_default();
        let mut id = [0u16; 512];
        let _ = SetupDiGetDeviceInstanceIdW(set, &dev, Some(&mut id), None);
        println!("{:>3}%  {}  [{}]", buf[0], name, wide_to_string(&id));
    }
    if found == 0 {
        println!("(egyetlen eszköz sem ad Bluetooth akku property-t)");
    }
    let _ = SetupDiDestroyDeviceInfoList(set);
    Ok(())
}

unsafe fn hid_devices() -> windows::core::Result<()> {
    for path in hid_paths()? {
        hid_one(&path);
    }
    Ok(())
}

/// Az összes jelenlévő HID interfész útvonala, nullával lezárt UTF-16-ként.
pub unsafe fn hid_paths() -> windows::core::Result<Vec<Vec<u16>>> {
    let guid = HidD_GetHidGuid();
    let set = SetupDiGetClassDevsW(Some(&guid), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)?;
    let mut paths = Vec::new();
    for i in 0.. {
        let mut ifd = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        if SetupDiEnumDeviceInterfaces(set, None, &guid, i, &mut ifd).is_err() {
            break;
        }
        let mut need = 0u32;
        let _ = SetupDiGetDeviceInterfaceDetailW(set, &ifd, None, 0, Some(&mut need), None);
        // u64-es puffer, hogy a struktúra igazítása rendben legyen
        let mut buf = vec![0u64; (need as usize + 7) / 8];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        if SetupDiGetDeviceInterfaceDetailW(set, &ifd, Some(detail), need, None, None).is_err() {
            continue;
        }
        let path_ptr = (*detail).DevicePath.as_ptr();
        let len = (0..).take_while(|&k| *path_ptr.add(k) != 0).count();
        paths.push(std::slice::from_raw_parts(path_ptr, len + 1).to_vec());
    }
    let _ = SetupDiDestroyDeviceInfoList(set);
    Ok(paths)
}

/// Egy HID interfész alapadatai (0 jogosultsággal is lekérdezhető).
pub struct HidInfo {
    pub path: Vec<u16>,
    pub vid: u16,
    pub pid: u16,
    pub product: String,
    pub page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
}

pub unsafe fn hid_info(path: &[u16]) -> Option<HidInfo> {
    let h = open(path, 0)?;
    let mut attrs = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    let _ = HidD_GetAttributes(h, &mut attrs);
    let mut prod = [0u16; 128];
    let _ = HidD_GetProductString(h, prod.as_mut_ptr() as *mut c_void, (prod.len() * 2) as u32);
    let mut pp = PHIDP_PREPARSED_DATA::default();
    let ok = HidD_GetPreparsedData(h, &mut pp).as_bool();
    let mut caps = HIDP_CAPS::default();
    if ok {
        let _ = HidP_GetCaps(pp, &mut caps);
        let _ = HidD_FreePreparsedData(pp);
    }
    let _ = CloseHandle(h);
    ok.then(|| HidInfo {
        path: path.to_vec(),
        vid: attrs.VendorID,
        pid: attrs.ProductID,
        product: wide_to_string(&prod),
        page: caps.UsagePage,
        usage: caps.Usage,
        in_len: caps.InputReportByteLength,
        out_len: caps.OutputReportByteLength,
    })
}

pub unsafe fn all_hid_info() -> Vec<HidInfo> {
    hid_paths().unwrap_or_default().iter().filter_map(|p| hid_info(p)).collect()
}

fn open(path: &[u16], access: u32) -> Option<HANDLE> {
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()
    }
}

fn is_battery_page(page: u16) -> bool {
    matches!(page, 0x06 | 0x84 | 0x85)
}

fn report_type_name(t: HIDP_REPORT_TYPE) -> &'static str {
    match t {
        HidP_Input => "input",
        HidP_Output => "output",
        _ => "feature",
    }
}

unsafe fn hid_one(path: &[u16]) {
    let path_str = wide_to_string(path);
    // Először írás-olvasással, ha nem megy (billentyűzet/egér kollekciót a rendszer foglalja), 0 jogosultsággal.
    let (h, rw) = match open(path, (GENERIC_READ | GENERIC_WRITE).0) {
        Some(h) => (h, true),
        None => match open(path, 0) {
            Some(h) => (h, false),
            None => {
                println!("- nem nyitható meg: {path_str}");
                return;
            }
        },
    };

    let mut attrs = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    let _ = HidD_GetAttributes(h, &mut attrs);
    let mut prod = [0u16; 128];
    let mut manu = [0u16; 128];
    let _ = HidD_GetProductString(h, prod.as_mut_ptr() as *mut c_void, (prod.len() * 2) as u32);
    let _ = HidD_GetManufacturerString(h, manu.as_mut_ptr() as *mut c_void, (manu.len() * 2) as u32);

    let mut pp = PHIDP_PREPARSED_DATA::default();
    if !HidD_GetPreparsedData(h, &mut pp).as_bool() {
        println!("- {:04X}:{:04X} preparsed data nem elérhető", attrs.VendorID, attrs.ProductID);
        let _ = CloseHandle(h);
        return;
    }
    let mut caps = HIDP_CAPS::default();
    let _ = HidP_GetCaps(pp, &mut caps);

    let vendor_page = caps.UsagePage >= 0xFF00;
    println!(
        "- {:04X}:{:04X}  \"{}\" / \"{}\"  page 0x{:04X} usage 0x{:04X}  in/out/feat {}/{}/{}  {}",
        attrs.VendorID,
        attrs.ProductID,
        wide_to_string(&manu),
        wide_to_string(&prod),
        caps.UsagePage,
        caps.Usage,
        caps.InputReportByteLength,
        caps.OutputReportByteLength,
        caps.FeatureReportByteLength,
        if rw { "[rw]" } else { "[csak attr]" },
    );
    println!("    {path_str}");

    for (rt, count) in [
        (HidP_Input, caps.NumberInputValueCaps),
        (HidP_Output, caps.NumberOutputValueCaps),
        (HidP_Feature, caps.NumberFeatureValueCaps),
    ] {
        if count == 0 {
            continue;
        }
        let mut vcaps = vec![HIDP_VALUE_CAPS::default(); count as usize];
        let mut n = count;
        if HidP_GetValueCaps(rt, vcaps.as_mut_ptr(), &mut n, pp).is_err() {
            continue;
        }
        let rlen = match rt {
            HidP_Input => caps.InputReportByteLength,
            HidP_Output => caps.OutputReportByteLength,
            _ => caps.FeatureReportByteLength,
        };
        for vc in &vcaps[..n as usize] {
            let usage = if vc.IsRange.as_bool() { vc.Anonymous.Range.UsageMin } else { vc.Anonymous.NotRange.Usage };
            if is_battery_page(vc.UsagePage) {
                print!(
                    "    ** AKKU usage: {} report 0x{:02X} page 0x{:02X} usage 0x{:02X} ({} bit, {}..{})",
                    report_type_name(rt), vc.ReportID, vc.UsagePage, usage, vc.BitSize, vc.LogicalMin, vc.LogicalMax
                );
                if rt != HidP_Output {
                    match read_value(h, pp, rt, vc.ReportID, rlen, vc.UsagePage, vc.LinkCollection, usage) {
                        Some(v) => println!("  => érték: {v}"),
                        None => println!("  => nem olvasható"),
                    }
                } else {
                    println!();
                }
            } else if vendor_page {
                println!(
                    "    {} report 0x{:02X}: {} x {} bit (page 0x{:04X} usage 0x{:02X})",
                    report_type_name(rt), vc.ReportID, vc.ReportCount, vc.BitSize, vc.UsagePage, usage
                );
            }
        }
    }

    // Gyártóspecifikus kollekciónál a "button" típusú reportok ID-i is (pl. bitmezős parancsok).
    if vendor_page {
        for (rt, count) in [
            (HidP_Input, caps.NumberInputButtonCaps),
            (HidP_Output, caps.NumberOutputButtonCaps),
            (HidP_Feature, caps.NumberFeatureButtonCaps),
        ] {
            if count == 0 {
                continue;
            }
            let mut bcaps = vec![HIDP_BUTTON_CAPS::default(); count as usize];
            let mut n = count;
            if HidP_GetButtonCaps(rt, bcaps.as_mut_ptr(), &mut n, pp).is_ok() {
                for bc in &bcaps[..n as usize] {
                    println!("    {} report 0x{:02X}: button caps (page 0x{:04X})", report_type_name(rt), bc.ReportID, bc.UsagePage);
                }
            }
        }
    }

    let _ = HidD_FreePreparsedData(pp);
    let _ = CloseHandle(h);
}

#[allow(clippy::too_many_arguments)]
unsafe fn read_value(
    h: HANDLE,
    pp: PHIDP_PREPARSED_DATA,
    rt: HIDP_REPORT_TYPE,
    report_id: u8,
    len: u16,
    page: u16,
    link: u16,
    usage: u16,
) -> Option<u32> {
    let mut report = vec![0u8; len as usize];
    report[0] = report_id;
    let ok = if rt == HidP_Feature {
        HidD_GetFeature(h, report.as_mut_ptr() as *mut c_void, len as u32)
    } else {
        HidD_GetInputReport(h, report.as_mut_ptr() as *mut c_void, len as u32)
    };
    if !ok.as_bool() {
        return None;
    }
    let mut value = 0u32;
    HidP_GetUsageValue(rt, page, link, usage, &mut value, pp, &mut report).ok().ok()?;
    Some(value)
}
