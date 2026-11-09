//! Eszközszál: providerek futtatása, eszközváltozások figyelése, csatorna a UI felé.
//!
//! A szál nem pollol: egy eseményre alszik, amit a Windows HID eszközértesítése jelez
//! (CM_Register_Notification). Ébredés után rövid ideig gyűjti a további értesítéseket
//! (egy eszköz több interfésszel jelenik meg), majd a providerek újraolvassák az állapotot,
//! és csak a ténylegesen változott eszközök mennek a UI szálnak.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid;
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::model::DeviceState;
use crate::providers::{wired_detect::WiredDetect, Provider};

/// A UI ablaknak küldött üzenet: új állapotok várnak a csatornán.
pub const WM_DEVICES: u32 = WM_APP + 2;

/// Ennyi ideig gyűjtjük az egymás utáni eszközértesítéseket egy újraolvasás előtt.
const SETTLE_MS: u32 = 300;

pub fn spawn(hwnd: HWND) -> Receiver<DeviceState> {
    let (tx, rx) = channel();
    let hwnd = hwnd.0 as isize; // a HWND nem Send
    thread::Builder::new()
        .name("devices".into())
        .spawn(move || unsafe { run(HWND(hwnd as *mut c_void), tx) })
        .expect("eszközszál indítása");
    rx
}

unsafe fn run(hwnd: HWND, tx: Sender<DeviceState>) {
    let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    let Ok(changed) = CreateEventW(None, false, false, PCWSTR::null()) else { return };
    let _notify = register_hid_notification(changed);

    let mut providers: Vec<Box<dyn Provider>> = vec![Box::new(WiredDetect::default())];
    let mut last: HashMap<String, DeviceState> = HashMap::new();
    loop {
        let mut any = false;
        for p in providers.iter_mut() {
            for state in p.scan() {
                if last.get(&state.id) != Some(&state) {
                    last.insert(state.id.clone(), state.clone());
                    if tx.send(state).is_err() {
                        return; // a UI kilépett
                    }
                    any = true;
                }
            }
        }
        if any {
            let _ = PostMessageW(hwnd, WM_DEVICES, WPARAM(0), LPARAM(0));
        }

        // Alvás a következő eszközváltozásig, majd a további értesítések kivárása.
        WaitForSingleObject(changed, INFINITE);
        while WaitForSingleObject(changed, SETTLE_MS) == WAIT_OBJECT_0 {}
    }
}

/// HID interfész megjelenés/eltűnés értesítés; a visszahívás csak jelzi az eseményt.
/// A visszaadott regisztráció a szál végéig él.
unsafe fn register_hid_notification(event: HANDLE) -> Option<HCMNOTIFICATION> {
    let mut filter = CM_NOTIFY_FILTER {
        cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
        FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        ..Default::default()
    };
    filter.u.DeviceInterface.ClassGuid = HidD_GetHidGuid();
    let mut handle = HCMNOTIFICATION::default();
    let cr = CM_Register_Notification(&filter, Some(event.0 as *const c_void), Some(on_device_change), &mut handle);
    (cr == CR_SUCCESS).then_some(handle)
}

unsafe extern "system" fn on_device_change(
    _notify: HCMNOTIFICATION,
    context: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    let _ = SetEvent(HANDLE(context as *mut c_void));
    ERROR_SUCCESS.0
}
