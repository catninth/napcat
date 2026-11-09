//! Tálcaikonok (Shell_NotifyIconW): eszközönként egy, egy rejtett ablakkal és jobb klikkes menüvel.

use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;

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
use crate::worker::{self, WM_DEVICES};

const WM_TRAY: u32 = WM_APP + 1;
const ID_EXIT: usize = 100;

/// Alap GUID; eszközönként ebből és az eszköz azonosítójából képzünk stabil GUID-ot, hogy a
/// Windows megjegyezze, melyik ikon van kitűzve. A GUID az exe útvonalához kötődik, ezért ha egy
/// másik helyről futó build már lefoglalta (pl. target\debug vs. target\release), uID-vel regisztrálunk.
const BASE_GUID: u128 = 0x6c1f3a52_8d4e_4b7a_9f21_3e5a7c0d9b14;

/// Explorer újraindulásakor kapott üzenet, erre újra kell regisztrálni az ikonokat.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

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

impl IconKey {
    fn new(d: &DeviceState, theme: Theme, size: u32) -> IconKey {
        IconKey { level: d.level.map(|l| (l + 2) / 5 * 5), charge: d.charge, connected: d.connected, kind: d.kind, theme, size }
    }
}

struct TrayIcon {
    uid: u32,
    guid: GUID,
    use_guid: bool,
    state: DeviceState,
    icon: HICON,
    key: Option<IconKey>,
}

impl TrayIcon {
    fn new(uid: u32, state: DeviceState) -> TrayIcon {
        let mut h = DefaultHasher::new();
        state.id.hash(&mut h);
        let guid = GUID::from_u128(BASE_GUID ^ h.finish() as u128);
        TrayIcon { uid, guid, use_guid: true, state, icon: HICON::default(), key: None }
    }

    /// Helyőrző, amíg egyetlen ismert eszköz sincs (különben nem lenne mire jobb klikkelni).
    fn placeholder() -> TrayIcon {
        let state = DeviceState {
            id: String::new(),
            name: "NapCat".into(),
            kind: DeviceKind::Other,
            level: None,
            charge: ChargeState::Unknown,
            connected: false,
        };
        TrayIcon { guid: GUID::from_u128(BASE_GUID), ..TrayIcon::new(1, state) }
    }

    fn is_placeholder(&self) -> bool {
        self.state.id.is_empty()
    }

    fn tooltip(&self) -> String {
        if self.is_placeholder() {
            "NapCat – nem található ismert eszköz".into()
        } else {
            self.state.tooltip()
        }
    }

    fn data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: self.uid,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        if self.use_guid {
            nid.uFlags |= NIF_GUID;
            nid.guidItem = self.guid;
        }
        let tip: Vec<u16> = self.tooltip().encode_utf16().take(nid.szTip.len() - 1).collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        nid
    }

    unsafe fn add(&mut self, hwnd: HWND) {
        for use_guid in [self.use_guid, false] {
            self.use_guid = use_guid;
            let mut nid = self.data(hwnd);
            if Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
                return;
            }
        }
    }

    unsafe fn modify(&self, hwnd: HWND) {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data(hwnd));
    }

    unsafe fn remove(&mut self, hwnd: HWND) {
        let _ = Shell_NotifyIconW(NIM_DELETE, &self.data(hwnd));
        if !self.icon.is_invalid() {
            let _ = DestroyIcon(self.icon);
            self.icon = HICON::default();
        }
    }
}

struct App {
    icons: Vec<TrayIcon>,
    devices: Receiver<DeviceState>,
    next_uid: u32,
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

    APP.set(Some(App { icons: vec![TrayIcon::placeholder()], devices: worker::spawn(hwnd), next_uid: 2 }));
    with_app(|app| {
        render_changed(hwnd, app);
        for icon in app.icons.iter_mut() {
            icon.add(hwnd);
        }
    });

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with_borrow_mut(|app| {
        if let Some(app) = app {
            f(app)
        }
    });
}

/// Újrarajzolja azokat az ikonokat, amelyek állapota, témája vagy mérete változott.
/// A változott ikonok indexeit adja vissza.
unsafe fn render_changed(hwnd: HWND, app: &mut App) -> Vec<usize> {
    let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd)) as u32;
    let theme = Theme::current();
    // A rajzoló (D2D, DirectWrite, WIC) csak a rajzolás idejére él: ritkán kell, és így
    // üresjáratban nem foglal memóriát. Utána a fölösleges lapokat visszaadjuk a rendszernek.
    let mut renderer: Option<IconRenderer> = None;
    let mut changed = Vec::new();
    for (i, icon) in app.icons.iter_mut().enumerate() {
        let key = IconKey::new(&icon.state, theme, size);
        if icon.key == Some(key) {
            continue;
        }
        if renderer.is_none() {
            match IconRenderer::new() {
                Ok(r) => renderer = Some(r),
                Err(_) => break,
            }
        }
        if let Some(Ok(hicon)) = renderer.as_ref().map(|r| r.render(&icon.state, size, theme)) {
            if !icon.icon.is_invalid() {
                let _ = DestroyIcon(icon.icon);
            }
            icon.icon = hicon;
            icon.key = Some(key);
            changed.push(i);
        }
    }
    if renderer.take().is_some() {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
    changed
}

/// Az eszközszálról érkezett állapotok beolvasása: új eszköz új ikont kap, a meglévők frissülnek.
unsafe fn on_devices(hwnd: HWND) {
    with_app(|app| {
        let mut added = Vec::new();
        for state in app.devices.try_iter().collect::<Vec<_>>() {
            if let Some(icon) = app.icons.iter_mut().find(|i| i.state.id == state.id) {
                icon.state = state;
                continue;
            }
            if let Some(pos) = app.icons.iter().position(TrayIcon::is_placeholder) {
                app.icons.remove(pos).remove(hwnd);
            }
            app.icons.push(TrayIcon::new(app.next_uid, state));
            added.push(app.icons.len() - 1);
            app.next_uid += 1;
        }
        render_changed(hwnd, app);
        for (i, icon) in app.icons.iter_mut().enumerate() {
            // A tooltip akkor is változhat, ha a kép nem, ezért minden meglévőt frissítünk.
            if added.contains(&i) {
                icon.add(hwnd);
            } else {
                icon.modify(hwnd);
            }
        }
    });
}

unsafe fn show_menu(hwnd: HWND, x: i32, y: i32) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let title: Vec<u16> = concat!("NapCat ", env!("CARGO_PKG_VERSION"), "\0").encode_utf16().collect();
    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(title.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Kilépés"));
    // Előtérbe kell hozni, különben a menü nem tűnik el, ha mellé kattintanak.
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, x, y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
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
            WM_DEVICES => {
                on_devices(hwnd);
                LRESULT(0)
            }
            WM_COMMAND => {
                if wparam.0 & 0xFFFF == ID_EXIT {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            // Téma (ImmersiveColorSet), DPI vagy kijelző változás: újrarajzolás, ha kell.
            WM_SETTINGCHANGE | WM_DPICHANGED | WM_DISPLAYCHANGE => {
                with_app(|app| {
                    for i in render_changed(hwnd, app) {
                        app.icons[i].modify(hwnd);
                    }
                });
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                with_app(|app| {
                    for icon in app.icons.iter_mut() {
                        icon.remove(hwnd);
                    }
                });
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
                with_app(|app| {
                    render_changed(hwnd, app);
                    for icon in app.icons.iter_mut() {
                        icon.add(hwnd);
                    }
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
