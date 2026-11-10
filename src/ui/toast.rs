//! Windows toast értesítések gombokkal ("Szólj 30 perc múlva", "Elvetés").
//!
//! Csomagolatlan alkalmazásként az AUMID-t a HKCU\Software\Classes\AppUserModelId kulcs
//! regisztrálja (név, ikon). A gombok és a toast kattintása a futó folyamatba érkezik
//! (Activated), onnan WM_TOAST üzenettel a tálca ablakához. Ha a toast nem érhető el
//! (pl. kikapcsolt értesítések, régi rendszer), a hívó balloont mutat helyette.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::core::{IInspectable, Interface, Result, HSTRING, PCWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_TOAST: u32 = WM_APP + 7;
const AUMID: &str = "catninth.NapCat";

/// A toastokból érkezett, még fel nem dolgozott argumentumok ("open", "snooze:<id>").
static ACTIONS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Az értesítések neve és ikonja a Windowsban; indításkor egyszer.
pub fn register() {
    let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("NapCat")) else { return };
    let icon = dir.join("toast.png");
    if let Some(png) = png_from_ico(include_bytes!("../../res/napcat.ico"), 128) {
        let _ = std::fs::create_dir_all(&dir);
        if std::fs::read(&icon).ok().as_deref() != Some(png) {
            let _ = std::fs::write(&icon, png);
        }
    }
    let key: Vec<u16> = format!("Software\\Classes\\AppUserModelId\\{AUMID}").encode_utf16().chain(Some(0)).collect();
    for (name, value) in [("DisplayName", "NapCat".to_string()), ("IconUri", icon.display().to_string())] {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                REG_SZ.0,
                Some(value.as_ptr() as *const c_void),
                (value.len() * 2) as u32,
            );
        }
    }
}

/// A `size` px-es PNG kép egy PNG-tömörített ICO fájlból.
fn png_from_ico(ico: &[u8], size: u8) -> Option<&[u8]> {
    let count = u16::from_le_bytes(ico.get(4..6)?.try_into().ok()?) as usize;
    (0..count).find_map(|i| {
        let e = ico.get(6 + 16 * i..22 + 16 * i)?;
        let len = u32::from_le_bytes(e[8..12].try_into().ok()?) as usize;
        let offset = u32::from_le_bytes(e[12..16].try_into().ok()?) as usize;
        (e[0] == size).then(|| ico.get(offset..offset + len)).flatten()
    })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Toast a NapCat nevében. `snooze`: eszköz id, ekkor "Szólj 30 perc múlva" és "Elvetés" gombbal.
pub fn show(hwnd: HWND, title: &str, text: &str, snooze: Option<&str>) -> Result<()> {
    let actions = snooze.map_or(String::new(), |id| {
        format!(
            "<actions><action content=\"Szólj 30 perc múlva\" arguments=\"snooze:{}\"/>\
             <action content=\"Elvetés\" arguments=\"dismiss\" activationType=\"system\"/></actions>",
            escape(id)
        )
    });
    let xml = format!(
        "<toast launch=\"open\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{actions}</toast>",
        escape(title),
        escape(text)
    );
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    let target = hwnd.0 as isize;
    toast.Activated(&TypedEventHandler::new(move |_, args: &Option<IInspectable>| {
        if let Some(args) = args.as_ref().and_then(|a| a.cast::<ToastActivatedEventArgs>().ok()) {
            ACTIONS.lock().unwrap().push(args.Arguments()?.to_string());
            unsafe {
                let _ = PostMessageW(HWND(target as *mut c_void), WM_TOAST, WPARAM(0), LPARAM(0));
            }
        }
        Ok(())
    }))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
}

/// A WM_TOAST óta érkezett argumentumok.
pub fn take_actions() -> Vec<String> {
    std::mem::take(&mut *ACTIONS.lock().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_icon_png() {
        let png = png_from_ico(include_bytes!("../../res/napcat.ico"), 128).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    /// Kézi teszt: toastot mutat, és 30 mp-ig vár egy gombnyomásra (`cargo test activation -- --ignored --nocapture`).
    #[test]
    #[ignore = "toastot mutat, kattintásra vár"]
    fn activation() {
        use windows::core::w;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            register();
            let hwnd = CreateWindowExW(Default::default(), w!("STATIC"), None, WINDOW_STYLE(0), 0, 0, 0, 0, HWND_MESSAGE, None, None, None).unwrap();
            show(hwnd, "NapCat teszt", "Nyomd meg a Szólj gombot", Some("teszt")).unwrap();
            let start = std::time::Instant::now();
            let mut msg = MSG::default();
            while start.elapsed().as_secs() < 30 {
                while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_TOAST {
                        println!("WM_TOAST: {:?}", take_actions());
                        return;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            panic!("nem jött WM_TOAST");
        }
    }
}
