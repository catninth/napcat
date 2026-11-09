//! Tálcaikonok (Shell_NotifyIconW): eszközönként egy, egy rejtett ablakkal, jobb klikkes menüvel
//! és bal klikkes felugró panellel.

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

use super::flyout;
use super::icon_render::{IconRenderer, Layout};
use super::theme::Theme;
use crate::model::{ChargeState, DeviceKind, DeviceState};
use crate::settings::{self, Settings};
use crate::worker::{self, WM_DEVICES};

const WM_TRAY: u32 = WM_APP + 1;
const PLACEHOLDER_UID: u32 = 1;
/// Billentyűzettel kiválasztott ikon (NIN_SELECT | NINF_KEY); a windows crate nem definiálja.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

// Menüparancsok.
const ID_EXIT: usize = 100;
const ID_AUTOSTART: usize = 101;
const ID_NOTIFY_LOW: usize = 102;
const ID_HIDE: usize = 103;
const ID_SHOW_BASE: usize = 200;
const ID_STYLE_BASE: usize = 300;

/// Alacsony akku értesítés küszöbei (%), mindegyiknél egyszer szól.
const LOW_THRESHOLDS: [u8; 2] = [15, 5];

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
    layout: Layout,
}

impl IconKey {
    fn new(d: &DeviceState, theme: Theme, size: u32, layout: Layout) -> IconKey {
        let level = d.level.map(|l| (l + 2) / 5 * 5);
        IconKey { level, charge: d.charge, connected: d.connected, kind: d.kind, theme, size, layout }
    }
}

struct TrayIcon {
    uid: u32,
    guid: GUID,
    use_guid: bool,
    /// Regisztrálva van-e a tálcán (NIM_ADD megtörtént).
    shown: bool,
    state: DeviceState,
    icon: HICON,
    key: Option<IconKey>,
    /// A legalacsonyabb küszöb, amelyről már szóltunk (100: még semmiről).
    warned: u8,
}

impl TrayIcon {
    fn new(uid: u32, state: DeviceState) -> TrayIcon {
        let mut h = DefaultHasher::new();
        state.id.hash(&mut h);
        let guid = GUID::from_u128(BASE_GUID ^ h.finish() as u128);
        TrayIcon { uid, guid, use_guid: true, shown: false, state, icon: HICON::default(), key: None, warned: 100 }
    }

    /// Helyőrző, amíg egyetlen látható eszköz sincs (különben nem lenne mire kattintani).
    fn placeholder() -> TrayIcon {
        let state = DeviceState::disconnected("", "NapCat", DeviceKind::Other);
        TrayIcon { guid: GUID::from_u128(BASE_GUID), ..TrayIcon::new(PLACEHOLDER_UID, state) }
    }

    fn is_placeholder(&self) -> bool {
        self.state.id.is_empty()
    }

    fn tooltip(&self) -> String {
        if self.is_placeholder() {
            "NapCat – nincs megjeleníthető eszköz".into()
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
        copy_wide(&mut nid.szTip, &self.tooltip());
        nid
    }

    unsafe fn add(&mut self, hwnd: HWND) {
        for use_guid in [self.use_guid, false] {
            self.use_guid = use_guid;
            let mut nid = self.data(hwnd);
            if Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
                self.shown = true;
                return;
            }
        }
    }

    unsafe fn modify(&self, hwnd: HWND) {
        if self.shown {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data(hwnd));
        }
    }

    unsafe fn remove(&mut self, hwnd: HWND) {
        if self.shown {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data(hwnd));
            self.shown = false;
        }
        if !self.icon.is_invalid() {
            let _ = DestroyIcon(self.icon);
            self.icon = HICON::default();
            self.key = None;
        }
    }

    /// Windows értesítés (balloon/toast) ennek az ikonnak a nevében.
    unsafe fn notify(&self, hwnd: HWND, title: &str, text: &str) {
        if !self.shown {
            return;
        }
        let mut nid = self.data(hwnd);
        nid.uFlags |= NIF_INFO;
        nid.dwInfoFlags = NIIF_WARNING;
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }

    /// Új állapotnál eldönti, kell-e alacsony akku értesítés; a küszöböt eszközönként egyszer jelzi,
    /// töltéskor vagy jelentős visszatöltődéskor újra élesedik.
    fn low_battery_crossed(&mut self) -> Option<u8> {
        let d = &self.state;
        let Some(level) = d.level.filter(|_| d.connected && d.charge == ChargeState::Discharging) else {
            if d.is_charging() {
                self.warned = 100;
            }
            return None;
        };
        if level > LOW_THRESHOLDS[0] + 5 {
            self.warned = 100;
        }
        let crossed = LOW_THRESHOLDS.iter().rev().copied().find(|&t| level <= t && self.warned > t)?;
        self.warned = crossed;
        Some(crossed)
    }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let src: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..src.len()].copy_from_slice(&src);
    dst[src.len()] = 0;
}

struct App {
    /// Minden ebben a munkamenetben látott eszköz, a rejtettek is.
    devices: Vec<TrayIcon>,
    placeholder: TrayIcon,
    receiver: Receiver<DeviceState>,
    settings: Settings,
    next_uid: u32,
}

impl App {
    fn icons_mut(&mut self) -> impl Iterator<Item = &mut TrayIcon> {
        self.devices.iter_mut().chain(std::iter::once(&mut self.placeholder))
    }

    fn device_states(&self) -> Vec<DeviceState> {
        self.devices.iter().map(|d| d.state.clone()).collect()
    }

    /// Melyik ikon látszódjon: a nem rejtett eszközök, és a helyőrző, ha egy sem látszik.
    unsafe fn sync(&mut self, hwnd: HWND) {
        for d in self.devices.iter_mut() {
            let visible = !self.settings.hidden.contains(&d.state.id);
            if !visible && d.shown {
                d.remove(hwnd);
            }
        }
        let any_visible = self.devices.iter().any(|d| !self.settings.hidden.contains(&d.state.id));
        if any_visible && self.placeholder.shown {
            self.placeholder.remove(hwnd);
        }
        render_changed(hwnd, self);
        let hidden = self.settings.hidden.clone();
        for d in self.devices.iter_mut().filter(|d| !hidden.contains(&d.state.id)) {
            if d.shown {
                d.modify(hwnd); // a tooltip akkor is változhat, ha a kép nem
            } else {
                d.add(hwnd);
            }
        }
        if !any_visible && !self.placeholder.shown {
            self.placeholder.add(hwnd);
        }
    }
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

    APP.set(Some(App {
        devices: Vec::new(),
        placeholder: TrayIcon::placeholder(),
        receiver: worker::spawn(hwnd),
        settings: Settings::load(),
        next_uid: PLACEHOLDER_UID + 1,
    }));
    with_app(|app| app.sync(hwnd));

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

fn with_app<R: Default>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.with_borrow_mut(|app| app.as_mut().map(f).unwrap_or_default())
}

/// Újrarajzolja a látható (vagy épp megjelenítendő) ikonokat, ha az állapotuk, a téma vagy a
/// méret változott. true, ha bármelyik változott.
unsafe fn render_changed(hwnd: HWND, app: &mut App) -> bool {
    let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd)) as u32;
    let theme = Theme::current();
    let layout = Layout::from_key(&app.settings.icon_style);
    let hidden = app.settings.hidden.clone();
    let any_visible = app.devices.iter().any(|d| !hidden.contains(&d.state.id));
    // A rajzoló (D2D, DirectWrite, WIC) csak a rajzolás idejére él: ritkán kell, és így
    // üresjáratban nem foglal memóriát. Utána a fölösleges lapokat visszaadjuk a rendszernek.
    let mut renderer: Option<IconRenderer> = None;
    let mut changed = false;
    for icon in app.icons_mut() {
        let wanted = if icon.is_placeholder() { !any_visible } else { !hidden.contains(&icon.state.id) };
        let key = IconKey::new(&icon.state, theme, size, layout);
        if !wanted || icon.key == Some(key) {
            continue;
        }
        if renderer.is_none() {
            match IconRenderer::new() {
                Ok(mut r) => {
                    r.tray_layout = layout;
                    renderer = Some(r)
                }
                Err(_) => break,
            }
        }
        if let Some(Ok(hicon)) = renderer.as_ref().map(|r| r.render(&icon.state, size, theme)) {
            if !icon.icon.is_invalid() {
                let _ = DestroyIcon(icon.icon);
            }
            icon.icon = hicon;
            icon.key = Some(key);
            changed = true;
        }
    }
    if renderer.take().is_some() {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
    changed
}

/// Az eszközszálról érkezett állapotok beolvasása: új eszköz új ikont kap, a meglévők frissülnek.
unsafe fn on_devices(hwnd: HWND) {
    let states = with_app(|app| {
        let mut warnings = Vec::new();
        for state in app.receiver.try_iter().collect::<Vec<_>>() {
            let index = match app.devices.iter().position(|d| d.state.id == state.id) {
                Some(i) => {
                    app.devices[i].state = state;
                    i
                }
                None => {
                    app.devices.push(TrayIcon::new(app.next_uid, state));
                    app.next_uid += 1;
                    app.devices.len() - 1
                }
            };
            if let Some(t) = app.devices[index].low_battery_crossed() {
                warnings.push((index, t));
            }
        }
        app.sync(hwnd);
        if app.settings.notify_low {
            for (i, threshold) in warnings {
                let d = &app.devices[i];
                let text = format!("{}: {}% maradt.", d.state.name, d.state.level.unwrap_or(threshold));
                d.notify(hwnd, &format!("Alacsony akkuszint ({threshold}%)"), &text);
            }
        }
        app.device_states()
    });
    flyout::update(&states);
}

/// A kattintott ikon (uID) helye a tálcán, a felugró panel elhelyezéséhez.
unsafe fn icon_rect(hwnd: HWND, uid: u32) -> Option<RECT> {
    let (guid, use_guid) = with_app(|app| {
        app.icons_mut().find(|i| i.uid == uid).map(|i| (i.guid, i.use_guid))
    })?;
    let id = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hwnd,
        uID: uid,
        guidItem: if use_guid { guid } else { GUID::zeroed() },
    };
    Shell_NotifyIconGetRect(&id).ok()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn show_menu(hwnd: HWND, uid: u32, x: i32, y: i32) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let (settings, clicked, hidden_names) = with_app(|app| {
        let clicked = app.devices.iter().find(|d| d.uid == uid).map(|d| d.state.name.clone());
        let hidden: Vec<(usize, String)> = app
            .devices
            .iter()
            .enumerate()
            .filter(|(_, d)| app.settings.hidden.contains(&d.state.id))
            .map(|(i, d)| (i, d.state.name.clone()))
            .collect();
        Some((app.settings.clone(), clicked, hidden))
    })
    .unwrap_or_default();
    // A menüszövegeknek a TrackPopupMenu végéig élniük kell.
    let mut texts: Vec<Vec<u16>> = Vec::new();
    let mut text = |s: String| {
        texts.push(wide(&s));
        PCWSTR(texts.last().unwrap().as_ptr())
    };
    let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };

    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, text(format!("NapCat {}", env!("CARGO_PKG_VERSION"))));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    if let Some(name) = clicked {
        let _ = AppendMenuW(menu, MF_STRING, ID_HIDE, text(format!("{name} elrejtése")));
    }
    if !hidden_names.is_empty() {
        if let Ok(sub) = CreatePopupMenu() {
            for (i, name) in &hidden_names {
                let _ = AppendMenuW(sub, MF_STRING, ID_SHOW_BASE + i, text(name.clone()));
            }
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Rejtett eszközök megjelenítése"));
        }
    }
    if let Ok(sub) = CreatePopupMenu() {
        for (i, (_, key, label)) in Layout::ALL.iter().enumerate() {
            let _ = AppendMenuW(sub, MF_STRING | check(settings.icon_style == *key), ID_STYLE_BASE + i, text(label.to_string()));
        }
        let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Ikon stílusa"));
    }
    let _ = AppendMenuW(menu, MF_STRING | check(settings::autostart_enabled()), ID_AUTOSTART, w!("Indítás a Windows-zal"));
    let _ = AppendMenuW(menu, MF_STRING | check(settings.notify_low), ID_NOTIFY_LOW, w!("Értesítés alacsony akkuszintnél"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Kilépés"));

    // Előtérbe kell hozni, különben a menü nem tűnik el, ha mellé kattintanak.
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_NONOTIFY, x, y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu); // az almenüt is felszabadítja
    drop(texts);
    on_command(hwnd, uid, cmd.0 as usize);
}

unsafe fn on_command(hwnd: HWND, uid: u32, cmd: usize) {
    match cmd {
        0 => {}
        ID_EXIT => {
            let _ = DestroyWindow(hwnd);
        }
        ID_AUTOSTART => settings::set_autostart(!settings::autostart_enabled()),
        ID_NOTIFY_LOW => with_app(|app| {
            app.settings.notify_low = !app.settings.notify_low;
            app.settings.save();
        }),
        ID_HIDE => with_app(|app| {
            if let Some(d) = app.devices.iter().find(|d| d.uid == uid) {
                app.settings.hidden.push(d.state.id.clone());
                app.settings.save();
                app.sync(hwnd);
            }
        }),
        _ if cmd >= ID_STYLE_BASE => with_app(|app| {
            if let Some((_, key, _)) = Layout::ALL.get(cmd - ID_STYLE_BASE) {
                app.settings.icon_style = key.to_string();
                app.settings.save();
                app.sync(hwnd);
            }
        }),
        _ => with_app(|app| {
            if let Some(d) = cmd.checked_sub(ID_SHOW_BASE).and_then(|i| app.devices.get(i)) {
                let id = d.state.id.clone();
                app.settings.hidden.retain(|h| *h != id);
                app.settings.save();
                app.sync(hwnd);
            }
        }),
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                // NOTIFYICON_VERSION_4: LOWORD(lParam) = esemény, HIWORD(lParam) = uID,
                // wParam = horgony koordináták.
                let event = lparam.0 as u32 & 0xFFFF;
                let uid = (lparam.0 as u32 >> 16) & 0xFFFF;
                match event {
                    WM_CONTEXTMENU => {
                        let x = (wparam.0 & 0xFFFF) as i16 as i32;
                        let y = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                        show_menu(hwnd, uid, x, y);
                    }
                    NIN_SELECT | NIN_KEYSELECT => {
                        if let Some(rect) = icon_rect(hwnd, uid) {
                            flyout::toggle(rect, with_app(|app| app.device_states()));
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DEVICES => {
                on_devices(hwnd);
                LRESULT(0)
            }
            // Téma (ImmersiveColorSet), DPI vagy kijelző változás: újrarajzolás, ha kell.
            WM_SETTINGCHANGE | WM_DPICHANGED | WM_DISPLAYCHANGE => {
                with_app(|app| app.sync(hwnd));
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                with_app(|app| {
                    for icon in app.icons_mut() {
                        icon.remove(hwnd);
                    }
                });
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
                with_app(|app| {
                    for icon in app.icons_mut() {
                        icon.shown = false; // az Explorer újraindult, az ikonok elvesztek
                    }
                    app.sync(hwnd);
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
