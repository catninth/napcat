//! Tálcaikon (Shell_NotifyIconW) egy rejtett ablakkal és jobb klikkes menüvel.

use std::cell::RefCell;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::core::{w, Result, GUID, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::icon_render::IconRenderer;
use super::theme::Theme;
use crate::model::{ChargeState, DeviceKind, DeviceState};

const WM_TRAY: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
const ID_EXIT: usize = 100;
const ID_TEST_BASE: usize = 200;

/// Stabil GUID, hogy a Windows megjegyezze, ki van-e tűzve az ikon.
/// A GUID az exe útvonalához kötődik, ezért ha egy másik helyről futó build már lefoglalta
/// (pl. target\debug vs. target\release), GUID nélkül, uID-vel regisztrálunk.
const ICON_GUID: GUID = GUID::from_u128(0x6c1f3a52_8d4e_4b7a_9f21_3e5a7c0d9b14);

/// Explorer újraindulásakor kapott üzenet, erre újra kell regisztrálni az ikont.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static USE_GUID: AtomicBool = AtomicBool::new(true);

/// Amitől az ikon képe függ. Csak akkor rajzolunk újra, ha ez változik;
/// a szintet 5%-ra kerekítve hasonlítjuk.
#[derive(Clone, Copy, PartialEq, Eq)]
struct IconKey {
    level: Option<u8>,
    charge: ChargeState,
    connected: bool,
    kind: DeviceKind,
    theme: Theme,
    size: u32,
}

struct App {
    device: DeviceState,
    icon: HICON,
    key: Option<IconKey>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub unsafe fn run(hinst: HINSTANCE) -> Result<()> {
    let class = w!("NapCatTray");
    let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class, ..Default::default() };
    RegisterClassW(&wc);
    // Rejtett felső szintű ablak (nem message-only, mert az nem kapja meg a TaskbarCreated üzenetet).
    let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("NapCat"), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, hinst, None)?;
    TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);

    // M1: fix teszteszköz, amíg nincs eszközszál.
    let device = (TEST_STATES[0].1)();
    APP.set(Some(App { device, icon: HICON::default(), key: None }));
    refresh(hwnd);
    add_icon(hwnd);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

/// Újrarajzolja az ikont, ha az állapot, a téma vagy a méret változott. true, ha változott.
unsafe fn refresh(hwnd: HWND) -> bool {
    let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd)) as u32;
    APP.with_borrow_mut(|app| {
        let Some(app) = app else { return false };
        let d = &app.device;
        let key = IconKey {
            level: d.level.map(|l| (l + 2) / 5 * 5),
            charge: d.charge,
            connected: d.connected,
            kind: d.kind,
            theme: Theme::current(),
            size,
        };
        if app.key == Some(key) {
            return false;
        }
        // A rajzoló (D2D, DirectWrite, WIC) csak a rajzolás idejére él: ritkán kell, és így
        // üresjáratban nem foglal memóriát. Utána a fölösleges lapokat visszaadjuk a rendszernek.
        let rendered = IconRenderer::new().and_then(|r| r.render(d, size, key.theme));
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
        match rendered {
            Ok(icon) => {
                if !app.icon.is_invalid() {
                    let _ = DestroyIcon(app.icon);
                }
                app.icon = icon;
                app.key = Some(key);
                true
            }
            Err(_) => false,
        }
    })
}

fn icon_data(hwnd: HWND, use_guid: bool) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    };
    if use_guid {
        nid.uFlags |= NIF_GUID;
        nid.guidItem = ICON_GUID;
    }
    nid
}

/// Teljes ikonadat a jelenlegi képpel és tooltippel.
fn full_icon_data(hwnd: HWND, use_guid: bool) -> NOTIFYICONDATAW {
    let mut nid = icon_data(hwnd, use_guid);
    nid.uFlags |= NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = WM_TRAY;
    APP.with_borrow(|app| {
        if let Some(app) = app {
            nid.hIcon = app.icon;
            let tip: Vec<u16> = app.device.tooltip().encode_utf16().take(nid.szTip.len() - 1).collect();
            nid.szTip[..tip.len()].copy_from_slice(&tip);
        }
    });
    nid
}

unsafe fn add_icon(hwnd: HWND) {
    for use_guid in [USE_GUID.load(Ordering::Relaxed), false] {
        let mut nid = full_icon_data(hwnd, use_guid);
        if Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            USE_GUID.store(use_guid, Ordering::Relaxed);
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
            return;
        }
    }
}

unsafe fn update_icon(hwnd: HWND) {
    let nid = full_icon_data(hwnd, USE_GUID.load(Ordering::Relaxed));
    let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
}

unsafe fn remove_icon(hwnd: HWND) {
    let nid = icon_data(hwnd, USE_GUID.load(Ordering::Relaxed));
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

fn test_device(kind: DeviceKind, level: Option<u8>, charge: ChargeState, connected: bool) -> DeviceState {
    DeviceState { id: "test".into(), name: "Teszt billentyűzet".into(), kind, level, charge, connected }
}

/// Ideiglenes (M1): a menüből kipróbálható állapotok, amíg nincsenek providerek.
type TestState = (&'static str, fn() -> DeviceState);
pub(crate) const TEST_STATES: &[TestState] = &[
    ("72%", || test_device(DeviceKind::Keyboard, Some(72), ChargeState::Discharging, true)),
    ("35%", || test_device(DeviceKind::Keyboard, Some(35), ChargeState::Discharging, true)),
    ("9%", || test_device(DeviceKind::Keyboard, Some(9), ChargeState::Discharging, true)),
    ("Tölt 52%", || test_device(DeviceKind::Keyboard, Some(52), ChargeState::Charging, true)),
    ("Tölt, ismeretlen szint", || test_device(DeviceKind::Keyboard, None, ChargeState::Charging, true)),
    ("Ismeretlen szint", || test_device(DeviceKind::Keyboard, None, ChargeState::Unknown, true)),
    ("100%", || test_device(DeviceKind::Keyboard, Some(100), ChargeState::Full, true)),
    ("Nincs kapcsolat", || test_device(DeviceKind::Keyboard, None, ChargeState::Unknown, false)),
    ("Egér 64%", || DeviceState { name: "Teszt egér".into(), ..test_device(DeviceKind::Mouse, Some(64), ChargeState::Discharging, true) }),
    ("Fejhallgató 46% tölt", || DeviceState { name: "Teszt fejhallgató".into(), ..test_device(DeviceKind::Headset, Some(46), ChargeState::Charging, true) }),
    ("Kontroller 12%", || DeviceState { name: "Teszt kontroller".into(), ..test_device(DeviceKind::Controller, Some(12), ChargeState::Discharging, true) }),
    ("Egyéb eszköz 88%", || DeviceState { name: "Teszt eszköz".into(), ..test_device(DeviceKind::Other, Some(88), ChargeState::Discharging, true) }),
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let title = wide(concat!("NapCat ", env!("CARGO_PKG_VERSION")));
    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(title.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    if let Ok(test) = CreatePopupMenu() {
        let labels: Vec<_> = TEST_STATES.iter().map(|(label, _)| wide(label)).collect();
        for (i, label) in labels.iter().enumerate() {
            let _ = AppendMenuW(test, MF_STRING, ID_TEST_BASE + i, PCWSTR(label.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_POPUP, test.0 as usize, w!("Teszt állapot"));
    }
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Kilépés"));
    // Előtérbe kell hozni, különben a menü nem tűnik el, ha mellé kattintanak.
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, x, y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu); // az almenüt is felszabadítja
}

unsafe fn redraw(hwnd: HWND) {
    if refresh(hwnd) {
        update_icon(hwnd);
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                // NOTIFYICON_VERSION_4: LOWORD(lParam) = esemény, wParam = horgony koordináták.
                if (lparam.0 as u32 & 0xFFFF) == WM_CONTEXTMENU {
                    let x = (wparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    show_menu(hwnd, x, y);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                if id == ID_EXIT {
                    let _ = DestroyWindow(hwnd);
                } else if let Some((_, make)) = id.checked_sub(ID_TEST_BASE).and_then(|i| TEST_STATES.get(i)) {
                    APP.with_borrow_mut(|app| {
                        if let Some(app) = app {
                            app.device = make();
                        }
                    });
                    // A tooltip akkor is változhat, ha a kép nem.
                    refresh(hwnd);
                    update_icon(hwnd);
                }
                LRESULT(0)
            }
            // Téma (ImmersiveColorSet), DPI vagy kijelző változás: újrarajzolás, ha kell.
            WM_SETTINGCHANGE | WM_DPICHANGED | WM_DISPLAYCHANGE => {
                redraw(hwnd);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                remove_icon(hwnd);
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
                refresh(hwnd);
                add_icon(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
