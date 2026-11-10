//! Tálcaikonok (Shell_NotifyIconW): eszközönként egy, egy rejtett ablakkal, jobb klikkes menüvel
//! és bal klikkes felugró panellel.

use std::cell::{Cell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;

use windows::core::{w, Result, GUID, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Power::{RegisterPowerSettingNotification, POWERBROADCAST_SETTING};
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_CONSOLE_DISPLAY_STATE};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use std::collections::HashMap;
use std::time::Instant;

use super::estimate::History;
use super::flyout;
use super::toast::{self, WM_TOAST};
use super::tooltip;
use super::settings_window;
use super::icon_render::{AppIcon, IconRenderer, Layout};
use super::theme::{self, Theme};
use crate::model::{ChargeState, DeviceKind, DeviceState};
use crate::providers::custom;
use crate::settings::{self, Settings};
use crate::updater;
use crate::worker::{self, WM_DEVICES, WM_UPDATE};

const WM_TRAY: u32 = WM_APP + 1;
const PLACEHOLDER_UID: u32 = 1;
/// Billentyűzettel kiválasztott ikon (NIN_SELECT | NINF_KEY); a windows crate nem definiálja.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;

// Menüparancsok.
const ID_EXIT: usize = 100;
const ID_AUTOSTART: usize = 101;
const ID_SETTINGS: usize = 102;
const ID_HIDE: usize = 103;
pub const ID_UPDATE: usize = 104;
const ID_SHOW_BASE: usize = 200;

/// A letöltött, ellenőrzött telepítő készen áll (READY).
const WM_UPDATE_READY: u32 = WM_APP + 4;
pub static INSTALLING: AtomicBool = AtomicBool::new(false);
/// Kézi frissítés-keresés kérése (a beállítások ablakból).
pub const WM_UPDATE_CHECK: u32 = WM_APP + 8;
/// A kézi keresés véget ért (a beállítások ablak újrarajzolásához).
const WM_UPDATE_CHECKED: u32 = WM_APP + 9;
static READY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Új verzió érkezett: automatikus módban telepít, különben értesít.
unsafe fn on_update(hwnd: HWND) {
    let Some(release) = updater::AVAILABLE.lock().ok().and_then(|a| a.clone()) else { return };
    let auto = with_app(|app| app.settings.update_mode == "auto");
    if auto {
        start_install(hwnd);
    } else {
        notify_app(hwnd, &format!("Új NapCat verzió: {}", release.version), "Telepítés: jobb klikk az ikonon → Frissítés telepítése.", false);
    }
}

/// Értesítés az első látható ikon nevében (eszköz vagy helyőrző).
unsafe fn notify_app(hwnd: HWND, title: &str, text: &str, warning: bool) {
    with_app(|app| {
        if let Some(icon) = app.icons_mut().find(|i| i.shown) {
            icon.notify(hwnd, title, text, warning);
        }
    });
}

/// Letöltés és ellenőrzés egy rövid életű szálon; a végén WM_UPDATE_READY.
unsafe fn start_install(hwnd: HWND) {
    let Some(release) = updater::AVAILABLE.lock().ok().and_then(|a| a.clone()) else { return };
    if INSTALLING.swap(true, Ordering::Relaxed) {
        return;
    }
    settings_window::refresh();
    let hwnd_raw = hwnd.0 as isize; // a HWND nem Send
    std::thread::spawn(move || {
        let path = updater::download_in_child(&release);
        if let Ok(mut ready) = READY.lock() {
            *ready = path;
        }
        unsafe {
            let _ = PostMessageW(HWND(hwnd_raw as *mut _), WM_UPDATE_READY, WPARAM(0), LPARAM(0));
        }
    });
}

unsafe fn on_update_ready(hwnd: HWND) {
    let path = READY.lock().ok().and_then(|mut r| r.take());
    match path {
        Some(path) if updater::run_installer(&path) => {
            // A telepítő csendben lefut, és a végén újraindítja az appot.
            let _ = DestroyWindow(hwnd);
        }
        _ => {
            INSTALLING.store(false, Ordering::Relaxed);
            settings_window::refresh();
            notify_app(hwnd, "A frissítés nem sikerült", "A letöltés vagy az ellenőrzés hibát adott; később újra próbálható.", true);
        }
    }
}

/// Kézi frissítés-keresés egy rövid életű szálon; talált kiadásnál WM_UPDATE, különben csak
/// a beállítások ablak frissül.
unsafe fn start_check(hwnd: HWND) {
    if updater::CHECKING.swap(true, Ordering::Relaxed) {
        return;
    }
    settings_window::refresh();
    let hwnd_raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        let found = updater::check_in_child(env!("CARGO_PKG_VERSION")).is_some();
        updater::CHECKING.store(false, Ordering::Relaxed);
        let msg = if found { WM_UPDATE } else { WM_UPDATE_CHECKED };
        unsafe {
            let _ = PostMessageW(HWND(hwnd_raw as *mut _), msg, WPARAM(0), LPARAM(0));
        }
    });
}

/// Alacsony akku értesítés második (kritikus) küszöbe; az első a beállításokból jön.
const CRITICAL: u8 = 5;

/// Teljes képernyős alkalmazás (játék, film, bemutató) fut-e: ilyenkor a csendes mód vár.
unsafe fn user_busy() -> bool {
    matches!(SHQueryUserNotificationState(), Ok(s) if s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE)
}

/// Várakozó értesítések ellenőrzése (csendes módban) ennyi időnként.
const QUIET_TIMER: usize = 1;
/// A "Szólj 30 perc múlva" szundik ellenőrzése percenként, amíg van várakozó.
const SNOOZE_TIMER: usize = 2;
const SNOOZE: std::time::Duration = std::time::Duration::from_secs(30 * 60);

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
    /// A teli töltésről már szóltunk (merüléskor újra élesedik).
    full_notified: bool,
    /// "Egy ikon" módban az összes eszköz összesítése a tooltipben.
    summary: Option<String>,
    /// A provider által adott típus (a felhasználó felülírhatja a beállításokban).
    orig_kind: DeviceKind,
}

impl TrayIcon {
    fn new(uid: u32, state: DeviceState) -> TrayIcon {
        let mut h = DefaultHasher::new();
        state.id.hash(&mut h);
        let guid = GUID::from_u128(BASE_GUID ^ h.finish() as u128);
        TrayIcon {
            uid,
            guid,
            use_guid: true,
            shown: false,
            state,
            icon: HICON::default(),
            key: None,
            warned: 100,
            full_notified: true, // induláskor egy már teli eszközről nem szólunk
            summary: None,
            orig_kind: DeviceKind::Other,
        }
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
        if let Some(summary) = &self.summary {
            summary.clone()
        } else if self.is_placeholder() {
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
            // NIF_SHOWTIP nélkül a Windows nem a saját tooltipjét mutatja, hanem NIN_POPUPOPEN/CLOSE-t
            // küld: ilyenkor a design gazdag tooltipje jelenik meg. A szöveg (NIF_TIP) az akadálymentességhez marad.
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
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
    unsafe fn notify(&self, hwnd: HWND, title: &str, text: &str, warning: bool) {
        if !self.shown {
            return;
        }
        let mut nid = self.data(hwnd);
        nid.uFlags |= NIF_INFO;
        nid.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, text);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }

    /// Új állapotnál eldönti, kell-e alacsony akku értesítés; a küszöböt eszközönként egyszer jelzi,
    /// töltéskor vagy jelentős visszatöltődéskor újra élesedik.
    fn low_battery_crossed(&mut self, threshold: u8) -> Option<u8> {
        let d = &self.state;
        let Some(level) = d.level.filter(|_| d.connected && d.charge == ChargeState::Discharging) else {
            if d.is_charging() {
                self.warned = 100;
            }
            return None;
        };
        if level > threshold + 5 {
            self.warned = 100;
        }
        let crossed = [CRITICAL, threshold].into_iter().find(|&t| level <= t && self.warned > t)?;
        self.warned = crossed;
        Some(crossed)
    }

    /// A provider típusát megjegyzi, és a felhasználói felülírást alkalmazza (friss állapot után).
    fn apply_kind(&mut self, settings: &Settings) {
        self.orig_kind = self.state.kind;
        if let Some(k) = settings.kind_override(&self.state.id) {
            self.state.kind = custom::kind_from_str(k);
        }
    }

    /// Beállításváltozáskor: vissza a provider típusára, mielőtt az új felülírás érvényesül.
    fn restore_kind(&mut self) {
        self.state.kind = self.orig_kind;
    }

    /// Most lett teli (töltés vége vagy 100% töltés közben); merüléskor újra élesedik.
    fn full_reached(&mut self) -> bool {
        let d = &self.state;
        let full = d.connected && (d.charge == ChargeState::Full || (d.is_charging() && d.level == Some(100)));
        if d.connected && d.charge == ChargeState::Discharging {
            self.full_notified = false;
        }
        if full && !self.full_notified {
            self.full_notified = true;
            return true;
        }
        false
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
    last_update: Option<std::time::Instant>,
    /// Csendes módban (teljes képernyős alkalmazás) várakozó értesítések: (eszköz id, cím, szöveg, figyelmeztetés).
    pending: Vec<(String, String, String, bool)>,
    /// Eszközönkénti előzmények a becsléshez ("kb. 40 perc", "1 ó 10 p a telig", "2 órája").
    histories: HashMap<String, History>,
    /// Szundizott lemerülés figyelmeztetések: (eszköz id, mikor szóljon újra).
    snoozed: Vec<(String, Instant)>,
}

/// Sürgősség az "egy ikon" módhoz: lemerülő > töltő > legalacsonyabb szint > többi (kisebb = sürgősebb).
fn urgency(d: &DeviceState, threshold: u8) -> (u8, u8) {
    let level = d.level.unwrap_or(101);
    match (d.connected, d.charge, d.level) {
        (true, ChargeState::Discharging, Some(l)) if l < threshold.max(20) => (0, l),
        (true, ChargeState::Charging, _) => (1, level),
        (true, _, Some(l)) => (2, l),
        (true, _, None) => (3, 101),
        (false, _, _) => (4, 101),
    }
}

impl App {
    fn icons_mut(&mut self) -> impl Iterator<Item = &mut TrayIcon> {
        self.devices.iter_mut().chain(std::iter::once(&mut self.placeholder))
    }

    /// Az eszközök a beállított sorrendben (ami nincs benne, a végére, érkezési sorrendben). Ha a
    /// tálcán látszók sorrendje változik, az ikonokat újra felvesszük, mert a Windows a hozzáadás
    /// sorrendjében mutatja őket; a következő sync már a helyes sorrendben adja hozzá őket.
    unsafe fn reorder(&mut self, hwnd: HWND) {
        let before: Vec<String> = self.devices.iter().map(|d| d.state.id.clone()).collect();
        let order = self.settings.order.clone();
        self.devices.sort_by_key(|d| order.iter().position(|o| *o == d.state.id).unwrap_or(usize::MAX));
        let after: Vec<String> = self.devices.iter().map(|d| d.state.id.clone()).collect();
        if before != after {
            for d in self.devices.iter_mut() {
                d.remove(hwnd);
            }
        }
    }

    /// Egy eszköz állapota a friss becsléssel (az idő múlásával a "2 órája" is változik).
    fn with_estimate(&self, d: &TrayIcon) -> DeviceState {
        let estimate = self.histories.get(&d.state.id).and_then(|h| h.estimate(&d.state, Instant::now()));
        DeviceState { estimate, ..d.state.clone() }
    }

    fn device_states(&self) -> Vec<DeviceState> {
        self.devices.iter().map(|d| self.with_estimate(d)).collect()
    }

    /// A NapCat-ben megjelenő eszközök (az Eszközök oldalon eltávolítottak nélkül).
    fn active_states(&self) -> Vec<DeviceState> {
        self.devices.iter().filter(|d| !self.settings.removed.contains(&d.state.id)).map(|d| self.with_estimate(d)).collect()
    }

    /// A tálcán látszó eszközök: a nem rejtettek, "egy ikon" módban csak a legsürgősebb.
    fn visible_ids(&self) -> Vec<String> {
        let s = &self.settings;
        let candidates = self.devices.iter().filter(|d| !s.hidden.contains(&d.state.id) && !s.removed.contains(&d.state.id));
        if self.settings.tray_mode == "one" {
            let threshold = self.settings.low_threshold;
            candidates.min_by_key(|d| urgency(&d.state, threshold)).map(|d| vec![d.state.id.clone()]).unwrap_or_default()
        } else {
            candidates.map(|d| d.state.id.clone()).collect()
        }
    }

    /// A beállítások alkalmazása (induláskor és a beállítások ablak változásainál).
    unsafe fn apply_settings(&mut self, hwnd: HWND) {
        let s = &self.settings;
        theme::configure(&s.theme, s.use_accent, s.custom_accent, s.ring_accent);
        worker::set_updates_enabled(s.update_mode != "off");
        worker::set_poll_secs(s.poll_secs);
        // Kézzel hozzáadott eszközök: a törölteket eltávolítjuk, az újakat az eszközszál olvassa be.
        custom::set(s.custom.clone());
        let custom_ids: Vec<String> = s.custom.iter().map(|c| c.id()).collect();
        let mut dropped = Vec::new();
        self.devices.retain_mut(|d| {
            let keep = !d.state.id.starts_with("custom-") || custom_ids.contains(&d.state.id);
            if !keep {
                dropped.push(d.uid);
                d.remove(hwnd);
            }
            keep
        });
        worker::refresh_now();
        let settings = self.settings.clone();
        for d in self.devices.iter_mut() {
            d.restore_kind();
            d.apply_kind(&settings);
        }
        for icon in self.icons_mut() {
            icon.key = None; // szín, elrendezés vagy típus változhatott: újrarajzolás
        }
        self.reorder(hwnd);
        self.sync(hwnd);
    }

    /// Melyik ikon látszódjon: a látható eszközök, és a helyőrző, ha egy sem látszik.
    unsafe fn sync(&mut self, hwnd: HWND) {
        let visible = self.visible_ids();
        // "Egy ikon" módban a tooltip minden (nem rejtett, nem eltávolított) eszközt felsorol.
        let summary = (self.settings.tray_mode == "one").then(|| {
            let s = &self.settings;
            let lines: Vec<String> = self
                .devices
                .iter()
                .filter(|d| !s.hidden.contains(&d.state.id) && !s.removed.contains(&d.state.id))
                .map(|d| d.state.tooltip())
                .collect();
            lines.join("\n")
        });
        for d in self.devices.iter_mut() {
            d.summary = summary.clone().filter(|_| visible.contains(&d.state.id));
            if !visible.contains(&d.state.id) && d.shown {
                d.remove(hwnd);
            }
        }
        if !visible.is_empty() && self.placeholder.shown {
            self.placeholder.remove(hwnd);
        }
        render_changed(hwnd, self, &visible);
        for d in self.devices.iter_mut().filter(|d| visible.contains(&d.state.id)) {
            if d.shown {
                d.modify(hwnd); // a tooltip akkor is változhat, ha a kép nem
            } else {
                d.add(hwnd);
            }
        }
        if visible.is_empty() && !self.placeholder.shown {
            self.placeholder.add(hwnd);
        }
    }

    /// Értesítés egy eszköz nevében; csendes módban teljes képernyős alkalmazás alatt vár.
    unsafe fn notify_device(&mut self, hwnd: HWND, id: &str, title: String, text: String, warning: bool) {
        if self.settings.quiet_fullscreen && user_busy() {
            self.pending.push((id.to_string(), title, text, warning));
            let _ = SetTimer(hwnd, QUIET_TIMER, 30_000, None);
            return;
        }
        // Toast gombokkal (lemerülésnél szundi); ha nem megy, balloon az eszköz ikonján.
        if toast::show(hwnd, &title, &text, warning.then_some(id)).is_ok() {
            return;
        }
        // Az eszköz ikonja, vagy ha az nem látszik (rejtett, "egy ikon" mód), az első látható ikon.
        let icon = match self.devices.iter().position(|d| d.state.id == id && d.shown) {
            Some(i) => &self.devices[i],
            None => match self.devices.iter().position(|d| d.shown) {
                Some(i) => &self.devices[i],
                None => &self.placeholder,
            },
        };
        icon.notify(hwnd, &title, &text, warning);
    }

    /// A várakozó értesítések kiküldése, ha már nincs teljes képernyős alkalmazás.
    unsafe fn flush_pending(&mut self, hwnd: HWND) {
        if self.pending.is_empty() || (self.settings.quiet_fullscreen && user_busy()) {
            return;
        }
        let _ = KillTimer(hwnd, QUIET_TIMER);
        for (id, title, text, warning) in std::mem::take(&mut self.pending) {
            self.notify_device(hwnd, &id, title, text, warning);
        }
    }

    /// Lejárt szundik: ha az eszköz még mindig lemerülőben van, újra szól.
    unsafe fn check_snoozed(&mut self, hwnd: HWND) {
        let now = Instant::now();
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.snoozed).into_iter().partition(|(_, t)| *t <= now);
        self.snoozed = rest;
        if self.snoozed.is_empty() {
            let _ = KillTimer(hwnd, SNOOZE_TIMER);
        }
        let threshold = self.settings.low_threshold;
        for (id, _) in due {
            let Some(d) = self.devices.iter().map(|d| &d.state).find(|d| d.id == id) else { continue };
            if let Some(level) = d.level.filter(|&l| d.connected && d.charge == ChargeState::Discharging && l <= threshold) {
                let (title, text) = low_battery_text(&d.name, level);
                self.notify_device(hwnd, &id, title, text, true);
            }
        }
    }
}

fn low_battery_text(name: &str, level: u8) -> (String, String) {
    (format!("{name} lemerülőben"), format!("{name}: {level}% maradt. Tedd töltőre."))
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static POWER: Cell<Power> = const { Cell::new(Power { display_off: false, locked: false, on_battery: false }) };
}

/// A képernyő, a munkamenet és az áramforrás állapota (Windows értesítésekből).
#[derive(Clone, Copy)]
struct Power {
    display_off: bool,
    locked: bool,
    on_battery: bool,
}

// WM_WTSSESSION_CHANGE események.
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;

fn update_power(f: impl FnOnce(&mut Power)) {
    let mut p = POWER.get();
    f(&mut p);
    POWER.set(p);
    worker::set_power(p.display_off || p.locked, p.on_battery);
}

/// Értesítés képernyő be/ki/halványításról és az áramforrásról (hálózat / akkumulátor);
/// regisztráláskor a Windows azonnal elküldi a jelenlegi állapotot is.
unsafe fn register_power_notifications(hwnd: HWND) {
    for guid in [GUID_CONSOLE_DISPLAY_STATE, GUID_ACDC_POWER_SOURCE] {
        let _ = RegisterPowerSettingNotification(HANDLE(hwnd.0), &guid, DEVICE_NOTIFY_WINDOW_HANDLE);
    }
    let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
}

unsafe fn on_power_setting(lparam: LPARAM) {
    let setting = &*(lparam.0 as *const POWERBROADCAST_SETTING);
    if setting.DataLength < 4 {
        return;
    }
    let value = u32::from_le_bytes(std::slice::from_raw_parts(setting.Data.as_ptr(), 4).try_into().unwrap());
    if setting.PowerSetting == GUID_CONSOLE_DISPLAY_STATE {
        update_power(|p| p.display_off = value == 0); // 0 ki, 1 be, 2 halványítva
    } else if setting.PowerSetting == GUID_ACDC_POWER_SOURCE {
        update_power(|p| p.on_battery = value != 0); // 0 hálózat, 1 akkumulátor, 2 UPS
    }
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
        last_update: None,
        pending: Vec::new(),
        histories: HashMap::new(),
        snoozed: Vec::new(),
    }));
    toast::register();
    with_app(|app| app.apply_settings(hwnd));
    register_power_notifications(hwnd);

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
unsafe fn render_changed(hwnd: HWND, app: &mut App, visible: &[String]) -> bool {
    let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd)) as u32;
    let theme = Theme::current();
    let layout = Layout::from_key(&app.settings.icon_style);
    // A rajzoló (D2D, DirectWrite, WIC) csak a rajzolás idejére él: ritkán kell, és így
    // üresjáratban nem foglal memóriát. Utána a fölösleges lapokat visszaadjuk a rendszernek.
    let mut renderer: Option<IconRenderer> = None;
    let mut changed = false;
    for icon in app.icons_mut() {
        let wanted = if icon.is_placeholder() { visible.is_empty() } else { visible.contains(&icon.state.id) };
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
        // A helyőrző (nincs megjeleníthető eszköz) az egyszínű NapCat macska.
        let rendered = renderer.as_ref().map(|r| {
            if icon.is_placeholder() { r.render_app_mark(size, theme) } else { r.render(&icon.state, size, theme) }
        });
        if let Some(Ok(hicon)) = rendered {
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
        let mut notes: Vec<(String, String, String, bool)> = Vec::new();
        let threshold = app.settings.low_threshold;
        for state in app.receiver.try_iter().collect::<Vec<_>>() {
            let index = match app.devices.iter().position(|d| d.state.id == state.id) {
                Some(i) => {
                    app.devices[i].state = state;
                    app.devices[i].apply_kind(&app.settings);
                    i
                }
                None => {
                    app.devices.push(TrayIcon::new(app.next_uid, state));
                    app.devices.last_mut().unwrap().apply_kind(&app.settings);
                    app.next_uid += 1;
                    app.devices.len() - 1
                }
            };
            let d = &mut app.devices[index];
            app.histories.entry(d.state.id.clone()).or_default().record(&d.state, Instant::now());
            if let Some(t) = d.low_battery_crossed(threshold) {
                if app.settings.notify_low {
                    let (title, text) = low_battery_text(&d.state.name, d.state.level.unwrap_or(t));
                    notes.push((d.state.id.clone(), title, text, true));
                }
            }
            if d.full_reached() && app.settings.notify_full {
                notes.push((d.state.id.clone(), format!("{} feltöltve", d.state.name), "100%, lecsatlakoztathatod a töltőről.".into(), false));
            }
        }
        app.reorder(hwnd);
        app.sync(hwnd);
        // Első indítás: a Windows 11 az új tálcaikonokat a rejtett területre teszi. Az első
        // eszközfrissítés után szólunk, hogy az értesítés egy maradandó ikonhoz tartozzon.
        if !app.settings.pin_tip_shown {
            app.settings.pin_tip_shown = true;
            app.settings.save();
            if let Some(icon) = app.icons_mut().find(|i| i.shown) {
                icon.notify(
                    hwnd,
                    "A NapCat fut",
                    "Ha nem látod az ikonokat, a tálca ^ gombja alatt vannak. Kitűzés: húzd őket a tálcára, \
                     vagy Beállítások › Személyre szabás › Tálca › Egyéb tálcaikonok.",
                    false,
                );
            }
        }
        for (id, title, text, warning) in notes {
            app.notify_device(hwnd, &id, title, text, warning);
        }
        app.flush_pending(hwnd);
        app.last_update = Some(std::time::Instant::now());
        (app.active_states(), app.device_states())
    });
    // Az app ikon "feltöltve" (zöld) állapota a panel és a beállítások logójához.
    AppIcon::update_charged(&states.0);
    flyout::update(&states.0, Some(0));
    settings_window::update_devices(&states.1);
}

/// A panel adatai: eszközök, nézet, hány másodperce volt az utolsó frissítés.
fn flyout_data() -> (Vec<DeviceState>, bool, Option<u64>) {
    APP.with_borrow(|app| match app {
        Some(app) => (app.active_states(), app.settings.flyout_grid, app.last_update.map(|t| t.elapsed().as_secs())),
        None => (Vec::new(), false, None),
    })
}

/// Toast gomb vagy kattintás: szundi, vagy a panel megnyitása az első látható ikonnál.
unsafe fn on_toast(hwnd: HWND) {
    for action in toast::take_actions() {
        if let Some(id) = action.strip_prefix("snooze:") {
            with_app(|app| app.snoozed.push((id.to_string(), Instant::now() + SNOOZE)));
            let _ = SetTimer(hwnd, SNOOZE_TIMER, 60_000, None);
        } else if action == "open" && !flyout::is_open() {
            let uid = with_app(|app| app.icons_mut().find(|i| i.shown).map(|i| i.uid));
            if let Some(rect) = uid.and_then(|uid| icon_rect(hwnd, uid)) {
                let (devices, grid, updated) = flyout_data();
                flyout::toggle(hwnd, rect, devices, grid, updated);
            }
        }
    }
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
    let (clicked, hidden_names) = with_app(|app| {
        let clicked = app.devices.iter().find(|d| d.uid == uid).map(|d| d.state.name.clone());
        let hidden: Vec<(usize, String)> = app
            .devices
            .iter()
            .enumerate()
            .filter(|(_, d)| app.settings.hidden.contains(&d.state.id))
            .map(|(i, d)| (i, d.state.name.clone()))
            .collect();
        Some((clicked, hidden))
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
    if let Some(release) = updater::AVAILABLE.lock().ok().and_then(|a| a.clone()) {
        let busy = if INSTALLING.load(Ordering::Relaxed) { MF_GRAYED } else { MF_ENABLED };
        let _ = AppendMenuW(menu, MF_STRING | busy, ID_UPDATE, text(format!("Frissítés telepítése ({})", release.version)));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Beállítások…"));
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
    let _ = AppendMenuW(menu, MF_STRING | check(settings::autostart_enabled()), ID_AUTOSTART, w!("Indítás a Windows-zal"));
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

unsafe fn open_settings(hwnd: HWND, page: settings_window::Page) {
    let devices = APP.with_borrow(|app| app.as_ref().map(|a| a.device_states()).unwrap_or_default());
    settings_window::open(hwnd, devices, page);
}

unsafe fn on_command(hwnd: HWND, uid: u32, cmd: usize) {
    match cmd {
        0 => {}
        ID_EXIT => {
            let _ = DestroyWindow(hwnd);
        }
        ID_AUTOSTART => settings::set_autostart(!settings::autostart_enabled()),
        ID_SETTINGS => open_settings(hwnd, settings_window::Page::Settings),
        ID_HIDE => with_app(|app| {
            if let Some(d) = app.devices.iter().find(|d| d.uid == uid) {
                app.settings.hidden.push(d.state.id.clone());
                app.settings.save();
                app.sync(hwnd);
            }
        }),
        ID_UPDATE => start_install(hwnd),
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
                        tooltip::hide();
                        let x = (wparam.0 & 0xFFFF) as i16 as i32;
                        let y = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
                        show_menu(hwnd, uid, x, y);
                    }
                    NIN_SELECT | NIN_KEYSELECT => {
                        tooltip::hide();
                        if let Some(rect) = icon_rect(hwnd, uid) {
                            let (devices, grid, updated) = flyout_data();
                            flyout::toggle(hwnd, rect, devices, grid, updated);
                        }
                        worker::request_refresh();
                    }
                    // Rámutatás: a design gazdag tooltipje (minden eszköz egy pillantásra); az egér
                    // ilyenkor ébren van, érdemes frissíteni is.
                    NIN_POPUPOPEN => {
                        worker::request_refresh();
                        if !flyout::is_open() {
                            if let Some(rect) = icon_rect(hwnd, uid) {
                                tooltip::show(rect, flyout_data().0);
                            }
                        }
                    }
                    NIN_POPUPCLOSE => tooltip::hide(),
                    WM_MOUSEMOVE => worker::request_refresh(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DEVICES => {
                on_devices(hwnd);
                LRESULT(0)
            }
            WM_UPDATE => {
                settings_window::refresh();
                on_update(hwnd);
                LRESULT(0)
            }
            WM_UPDATE_CHECK => {
                start_check(hwnd);
                LRESULT(0)
            }
            WM_UPDATE_CHECKED => {
                settings_window::refresh();
                LRESULT(0)
            }
            flyout::WM_FLYOUT => {
                match wparam.0 {
                    flyout::FLYOUT_GRID => with_app(|app| {
                        app.settings.flyout_grid = lparam.0 != 0;
                        app.settings.save();
                    }),
                    flyout::FLYOUT_SETTINGS => open_settings(hwnd, settings_window::Page::Settings),
                    flyout::FLYOUT_DEVICES => open_settings(hwnd, settings_window::Page::Devices),
                    _ => {}
                }
                LRESULT(0)
            }
            settings_window::WM_SETTINGS_CHANGED => {
                let states = with_app(|app| {
                    app.settings = Settings::load();
                    app.apply_settings(hwnd);
                    (app.active_states(), app.device_states())
                });
                // Eltávolított vagy törölt eszköz: a panel és az ablak listája is frissül.
                AppIcon::update_charged(&states.0);
                flyout::update(&states.0, None);
                settings_window::update_devices(&states.1);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == QUIET_TIMER => {
                with_app(|app| app.flush_pending(hwnd));
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == SNOOZE_TIMER => {
                with_app(|app| app.check_snoozed(hwnd));
                LRESULT(0)
            }
            WM_TOAST => {
                on_toast(hwnd);
                LRESULT(0)
            }
            WM_UPDATE_READY => {
                on_update_ready(hwnd);
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                if wparam.0 as u32 == PBT_POWERSETTINGCHANGE {
                    on_power_setting(lparam);
                }
                LRESULT(1)
            }
            WM_WTSSESSION_CHANGE => {
                match wparam.0 {
                    WTS_SESSION_LOCK => update_power(|p| p.locked = true),
                    WTS_SESSION_UNLOCK => update_power(|p| p.locked = false),
                    _ => {}
                }
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
