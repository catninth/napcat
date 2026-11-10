//! Beállítások ablak a design "Beállítások" artboardja szerint: Általános, Kiemelő szín, Eszközök a
//! tálcán, Értesítések, Teljesítmény. Direct2D-vel rajzolt vezérlők (kapcsoló, szegmensválasztó,
//! rádiógomb, színminta, csúszka); minden változás azonnal mentődik, és a tálca ablak alkalmazza.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::Ordering;

use windows::core::{w, Interface, Result, PCWSTR};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::anim::{self, fade, mix, Anims};
use super::draw::{contains, rect, Painter};
use super::icon_render::{AppIcon, IconRenderer, Layout};
use super::theme::{self, rgb, Theme};
use super::tray;
use crate::model::{ChargeState, DeviceKind, DeviceState};
use crate::providers::custom::{self, Candidate};
use crate::settings::{self, CustomDevice, Settings};
use crate::i18n::t;
use crate::updater;

/// Üzenet a tálca ablaknak: a beállítások megváltoztak (újraolvasás a fájlból).
pub const WM_SETTINGS_CHANGED: u32 = WM_APP + 6;

const W: f32 = 720.0;
const MARGIN: f32 = 32.0;
const SECTION_GAP: f32 = 26.0;
const ROW_GAP: f32 = 4.0;
/// Az eszközlista sorainak távolsága (sor + köz).
const PITCH: f32 = 52.0 + ROW_GAP;
/// Animációs képkocka időzítő.
const ANIM_TIMER: usize = 1;
/// A kurzor elhagyta az ablakot (TrackMouseEvent); a windows crate a Controls modulban tartja.
const WM_MOUSELEAVE: u32 = 0x02A3;

const SWATCHES: [(&str, u32); 7] = [
    ("Égkék", 0x4CC2FF),
    ("Levendula", 0xA78BFA),
    ("Rózsaszín", 0xF472B6),
    ("Narancs", 0xFB923C),
    ("Citrom", 0xFACC15),
    ("Menta", 0x34D399),
    ("Türkiz", 0x2DD4BF),
];

#[derive(Clone, PartialEq, Debug)]
enum Act {
    Startup,
    TrayMode(&'static str),
    Theme(&'static str),
    IconStyle(&'static str),
    Updates(&'static str),
    /// A felület nyelve: "auto", "hu" vagy "en".
    Language(&'static str),
    UseAccent,
    Source(bool),
    Swatch(u32),
    RingAccent,
    Device(String),
    NotifyLow,
    Threshold,
    NotifyFull,
    Quiet,
    Poll(u32),
    /// Oldalváltás (Beállítások / Eszközök).
    Page(Page),
    /// Eszköz eltávolítása a NapCat-ből (kézzel hozzáadottnál törlés, különben elrejtés mindenhonnan).
    Remove(String),
    /// Korábban eltávolított eszköz visszaadása.
    Restore(String),
    /// Észlelt, nem támogatott eszköz hozzáadása (csak csatlakozás).
    Add(usize),
    /// Új Bluetooth eszköz párosítása a Windowsban.
    PairBluetooth,
    /// Eszköztípus váltása (körbejár, végül vissza automatikusra).
    Kind(String),
    /// Az eszköz sorának fogantyúja: átrendezés húzással (az egérgomb lenyomásával indul).
    Drag(String),
    /// Frissítés keresése most.
    CheckUpdate,
    /// Az elérhető frissítés letöltése és telepítése.
    InstallUpdate,
}

fn kind_label(k: DeviceKind) -> &'static str {
    match k {
        DeviceKind::Keyboard => t("Billentyűzet", "Keyboard"),
        DeviceKind::Mouse => t("Egér", "Mouse"),
        DeviceKind::Headset => t("Fejhallgató", "Headset"),
        DeviceKind::Controller => t("Kontroller", "Controller"),
        DeviceKind::Other => t("Egyéb", "Other"),
    }
}

/// Az ablak oldalai.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Settings,
    Devices,
}

struct Window {
    hwnd: HWND,
    owner: HWND,
    page: Page,
    settings: Settings,
    autostart: bool,
    /// Minden ismert eszköz, az eltávolítottak is (azok az Eszközök oldalon visszaadhatók).
    devices: Vec<DeviceState>,
    /// A Windows által észlelt, a NapCat-ben nem szereplő eszközök.
    candidates: Vec<Candidate>,
    renderer: IconRenderer,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: f32,
    scroll: f32,
    content_h: f32,
    hits: Vec<(D2D_RECT_F, Act)>,
    /// A küszöb csúszka húzása közben a csúszka sávja (tartalom koordinátákban).
    dragging: Option<D2D_RECT_F>,
    /// A csúszka gombjának helye húzás közben (0..1, a kurzor alatt, lépésre igazítás nélkül).
    slider: Option<f32>,
    /// Az átrendezéshez húzott eszköz, a kurzor y-ja és a megfogás helye a soron belül.
    drag_device: Option<(String, f32, f32)>,
    /// A tálca eszközlistája az utolsó rajzolásból: felső y és az eszközök sorrendben.
    list: (f32, Vec<String>),
    anims: Anims,
    /// A kurzor helye (kliens DIP), a rámutatás kiemeléséhez.
    hover: Option<(f32, f32)>,
    /// A kurzor alatti kattintható elem indexe (változáskor újrarajzolás).
    hover_hit: Option<usize>,
    tracking: bool,
    /// Görgetősáv: a kurzor fölötte van-e, és húzás közben a kezdő kurzor y (kliens DIP) és görgetés.
    sb_hover: bool,
    sb_drag: Option<(f32, f32)>,
    /// A címsor és a tálcagomb ikonjának állapota (változáskor újrarajzolva); None: még nincs.
    icon_style: Option<AppIcon>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Megnyitja (vagy előtérbe hozza) az ablakot a kért oldalon.
pub unsafe fn open(owner: HWND, devices: Vec<DeviceState>, page: Page) {
    let existing = WINDOW.with_borrow_mut(|w| {
        let w = w.as_mut()?;
        set_page(w, page);
        Some(w.hwnd)
    });
    if let Some(hwnd) = existing {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
        return;
    }
    let _ = create(owner, devices, page);
}

unsafe fn set_page(w: &mut Window, page: Page) {
    w.page = page;
    w.scroll = 0.0;
    // Az új oldal beúszik.
    w.anims.set("page", 0.0);
    w.candidates = candidates_for(&w.devices);
    set_title(w.hwnd, page);
    let _ = InvalidateRect(w.hwnd, None, false);
}

/// Az ablak címe az oldal és a nyelv szerint.
unsafe fn set_title(hwnd: HWND, page: Page) {
    let name = if page == Page::Devices { t("Eszközök", "Devices") } else { t("Beállítások", "Settings") };
    let title: Vec<u16> = format!("NapCat – {name}").encode_utf16().chain(Some(0)).collect();
    let _ = SetWindowTextW(hwnd, PCWSTR(title.as_ptr()));
}

/// A hozzáadható eszközök: ami a Windowsban látszik, de egyik NapCat eszköz sem az (VID/PID szerint).
fn candidates_for(devices: &[DeviceState]) -> Vec<Candidate> {
    custom::candidates().into_iter().filter(|c| !devices.iter().any(|d| d.hw == Some((c.vid, c.pid)))).collect()
}

/// Friss eszközlista egy nyitott ablaknak.
pub unsafe fn update_devices(devices: &[DeviceState]) {
    WINDOW.with_borrow_mut(|w| {
        if let Some(w) = w {
            w.devices = devices.to_vec();
            if w.page == Page::Devices {
                w.candidates = candidates_for(&w.devices);
            }
            let _ = InvalidateRect(w.hwnd, None, false);
        }
    });
}

const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);

unsafe fn create(owner: HWND, devices: Vec<DeviceState>, page: Page) -> Result<()> {
    let hinst = GetModuleHandleW(None)?;
    let class = w!("NapCatSettings");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: class,
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        hIcon: LoadIconW(hinst, PCWSTR(1 as _)).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc);
    let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("NapCat"), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 100, 100, None, None, hinst, None)?;
    let dpi = GetDpiForWindow(hwnd) as f32;
    let settings = Settings::load();
    WINDOW.set(Some(Window {
        hwnd,
        owner,
        page,
        settings,
        autostart: settings::autostart_enabled(),
        candidates: Vec::new(),
        devices,
        renderer: IconRenderer::new()?,
        rt: None,
        dpi,
        scroll: 0.0,
        content_h: 1180.0,
        hits: Vec::new(),
        dragging: None,
        slider: None,
        drag_device: None,
        list: (0.0, Vec::new()),
        anims: Anims::default(),
        hover: None,
        hover_hit: None,
        sb_hover: false,
        sb_drag: None,
        tracking: false,
        icon_style: None,
    }));
    apply_frame(hwnd);
    place(hwnd, dpi);
    WINDOW.with_borrow_mut(|w| {
        if let Some(w) = w {
            set_page(w, page);
        }
    });
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    Ok(())
}

/// Címsor és tálcagomb ikon: az app ikon a pontos méretben rajzolva (a .ico kicsinyítése helyett),
/// így 16-24 px-en is látszik a macska és a "zz"; az állapot (sötét, világos, feltöltve) szerint.
unsafe fn set_window_icons(hwnd: HWND, dpi: f32, style: AppIcon) {
    let Ok(renderer) = IconRenderer::new() else { return };
    for (which, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
        let size = GetSystemMetricsForDpi(metric, dpi as u32) as u32;
        if let Ok(icon) = renderer.render_app_icon(size, style) {
            let old = SendMessageW(hwnd, WM_SETICON, WPARAM(which as usize), LPARAM(icon.0 as isize));
            if old.0 != 0 {
                let _ = DestroyIcon(HICON(old.0 as *mut c_void));
            }
        }
    }
}

/// Címsor a design szerint: sötét/világos mód, a címsor színe az ablak háttere.
unsafe fn apply_frame(hwnd: HWND) {
    let theme = Theme::ui();
    let set = |attr: DWMWINDOWATTRIBUTE, v: u32| {
        let _ = DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const c_void, size_of::<u32>() as u32);
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, (!theme.light) as u32);
    let bg = theme.window();
    let colorref = |c: D2D1_COLOR_F| ((c.b * 255.0) as u32) << 16 | ((c.g * 255.0) as u32) << 8 | (c.r * 255.0) as u32;
    set(DWMWA_CAPTION_COLOR, colorref(bg));
    set(DWMWA_TEXT_COLOR, colorref(theme.fg(1.0)));
}

/// Méret: 720 DIP széles kliensterület, a magasság a munkaterülethez igazítva; középre.
unsafe fn place(hwnd: HWND, dpi: f32) {
    let scale = dpi / 96.0;
    let mut work = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut RECT as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let max_h = (work.bottom - work.top) as f32 - 80.0 * scale;
    let mut r = RECT { left: 0, top: 0, right: (W * scale) as i32, bottom: (900.0 * scale).min(max_h) as i32 };
    let _ = AdjustWindowRectExForDpi(&mut r, STYLE, false, WINDOW_EX_STYLE(0), dpi as u32);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let x = work.left + ((work.right - work.left) - w) / 2;
    let y = work.top + ((work.bottom - work.top) - h) / 2;
    let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
}

/// Változás után: mentés és jelzés a tálca ablaknak.
unsafe fn changed(w: &mut Window) {
    w.settings.save();
    theme::configure(&w.settings.theme, w.settings.use_accent, w.settings.custom_accent, w.settings.ring_accent);
    let _ = PostMessageW(w.owner, WM_SETTINGS_CHANGED, WPARAM(0), LPARAM(0));
    let _ = InvalidateRect(w.hwnd, None, false);
}

unsafe fn on_act(w: &mut Window, act: Act) {
    let s = &mut w.settings;
    match act {
        Act::Startup => {
            settings::set_autostart(!w.autostart);
            w.autostart = settings::autostart_enabled();
        }
        Act::TrayMode(k) => s.tray_mode = k.into(),
        Act::Theme(k) => {
            s.theme = k.into();
            theme::configure(&s.theme, s.use_accent, s.custom_accent, s.ring_accent);
            apply_frame(w.hwnd);
        }
        Act::IconStyle(k) => s.icon_style = k.into(),
        Act::Updates(k) => s.update_mode = k.into(),
        Act::Language(k) => {
            s.language = k.into();
            crate::i18n::set(k);
            set_title(w.hwnd, w.page);
        }
        Act::UseAccent => s.use_accent = !s.use_accent,
        Act::Source(custom) => s.custom_accent = if custom { Some(s.custom_accent.unwrap_or(SWATCHES[1].1)) } else { None },
        Act::Swatch(c) => s.custom_accent = Some(c),
        Act::RingAccent => s.ring_accent = !s.ring_accent,
        Act::Device(id) => {
            if let Some(pos) = s.hidden.iter().position(|h| *h == id) {
                s.hidden.remove(pos);
            } else {
                s.hidden.push(id);
            }
        }
        Act::NotifyLow => s.notify_low = !s.notify_low,
        Act::Threshold => return, // a csúszkát a húzás kezeli
        Act::NotifyFull => s.notify_full = !s.notify_full,
        Act::Quiet => s.quiet_fullscreen = !s.quiet_fullscreen,
        Act::Poll(secs) => s.poll_secs = secs,
        Act::Page(page) => return set_page(w, page),
        Act::Remove(id) => {
            if let Some(pos) = s.custom.iter().position(|c| c.id() == id) {
                // A kézzel hozzáadott eszköz megszűnik, újra az észlelt eszközök között jelenik meg.
                s.custom.remove(pos);
                w.devices.retain(|d| d.id != id);
                w.candidates = candidates_for(&w.devices);
            } else if !s.removed.contains(&id) {
                s.removed.push(id.clone());
            }
            s.hidden.retain(|h| *h != id);
        }
        Act::Restore(id) => s.removed.retain(|r| *r != id),
        Act::Drag(_) => return, // a húzást az egérkezelés intézi
        Act::Kind(id) => {
            // Körbejár: automatikus -> billentyűzet -> egér -> fejhallgató -> kontroller -> egyéb -> automatikus.
            const ORDER: [&str; 5] = ["keyboard", "mouse", "headset", "controller", "other"];
            let next = match s.kind_override(&id) {
                None => Some(ORDER[0]),
                Some(k) => ORDER.iter().position(|o| *o == k).and_then(|i| ORDER.get(i + 1)).copied(),
            };
            s.kinds.retain(|(i, _)| *i != id);
            if let Some(k) = next {
                s.kinds.push((id, k.to_string()));
            }
        }
        Act::Add(i) => {
            let Some(c) = w.candidates.get(i).cloned() else { return };
            let custom = CustomDevice { vid: c.vid, pid: c.pid, product: c.product.clone(), kind: custom::kind_to_str(c.kind).into() };
            if !s.custom.iter().any(|x| x.id() == custom.id()) {
                s.custom.push(custom);
            }
            w.candidates.remove(i);
        }
        Act::PairBluetooth => {
            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), w!("ms-settings:bluetooth"), None, None, SW_SHOWNORMAL);
            return;
        }
        // A tálca ablaka végzi (ugyanaz, mint a menü parancsai); az állapotot ő jelzi vissza.
        Act::CheckUpdate => {
            let _ = PostMessageW(w.owner, tray::WM_UPDATE_CHECK, WPARAM(0), LPARAM(0));
            return;
        }
        Act::InstallUpdate => {
            let _ = PostMessageW(w.owner, WM_COMMAND, WPARAM(tray::ID_UPDATE), LPARAM(0));
            return;
        }
    }
    changed(w);
}

/// Újrarajzolás, ha nyitva van (pl. változott a frissítés állapota).
pub unsafe fn refresh() {
    WINDOW.with_borrow(|w| {
        if let Some(w) = w {
            let _ = InvalidateRect(w.hwnd, None, false);
        }
    });
}

/// A húzott sor helye a listában a sor teteje (`top`) szerint: a legközelebbi hely, 0..n-1.
fn drag_index(list_top: f32, n: usize, top: f32) -> usize {
    (((top - list_top) / PITCH).round().max(0.0) as usize).min(n.saturating_sub(1))
}

/// Az átrendezés befejezése: a húzott eszköz a kurzor alatti helyre kerül; az új sorrend mentődik.
unsafe fn drop_device(w: &mut Window, id: &str, top: f32) {
    let (list_top, mut ids) = w.list.clone();
    let to = drag_index(list_top, ids.len(), top);
    let Some(from) = ids.iter().position(|i| i == id) else { return };
    let moved = ids.remove(from);
    ids.insert(to.min(ids.len()), moved);
    // A teljes sorrend: a lista, utána a többi ismert eszköz (pl. eltávolítottak) változatlanul.
    for d in &w.devices {
        if !ids.contains(&d.id) {
            ids.push(d.id.clone());
        }
    }
    // Az ablak saját listája is azonnal az új sorrendben.
    w.devices.sort_by_key(|d| ids.iter().position(|i| *i == d.id).unwrap_or(usize::MAX));
    w.settings.order = ids;
    changed(w);
}

/// Csúszka érték a vízszintes pozícióból (5-30%, 5-ös lépés); a gomb a kurzort követi.
unsafe fn drag_threshold(w: &mut Window, x: f32, track: D2D_RECT_F) {
    let t = ((x - track.left) / (track.right - track.left)).clamp(0.0, 1.0);
    w.slider = Some(t);
    let value = (5.0 + (t * 5.0).round() * 5.0) as u8;
    if w.settings.low_threshold != value {
        w.settings.low_threshold = value;
        changed(w);
    }
    let _ = InvalidateRect(w.hwnd, None, false);
}

fn device_conn(d: &DeviceState) -> &'static str {
    match (d.connected, d.charge) {
        (false, _) => t("Nincs csatlakoztatva", "Not connected"),
        (true, ChargeState::Charging) => t("Csatlakoztatva · tölt", "Connected · charging"),
        (true, ChargeState::Full) => t("Csatlakoztatva · feltöltve", "Connected · fully charged"),
        _ => t("Csatlakoztatva", "Connected"),
    }
}

/// Rajzolás (vagy csak elrendezés) és a kattintható elemek; a tartalom magasságát adja vissza.
struct Ui<'a> {
    p: Painter<'a>,
    theme: Theme,
    accent: D2D1_COLOR_F,
    hits: Vec<(D2D_RECT_F, Act)>,
    /// Görgetés: minden rajz ennyivel feljebb kerül.
    scroll: f32,
    /// Éppen húzott eszköz, a kurzor y-ja és a megfogás helye (tartalom koordinátákban).
    drag: Option<(String, f32, f32)>,
    /// A tálca eszközlistája: felső y és az eszközök sorrendben (a húzás célhelyéhez).
    list: (f32, Vec<String>),
    anims: Anims,
    /// A kurzor helye tartalom koordinátákban.
    hover: Option<(f32, f32)>,
    /// A csúszka gombja húzás közben (0..1).
    slider: Option<f32>,
}

impl Ui<'_> {
    /// Rámutatás kiemelés (0..1) egy elemhez, simán be- és kiúszva.
    fn hover_anim(&mut self, key: &str, r: &D2D_RECT_F) -> f32 {
        let on = self.hover.is_some_and(|(x, y)| contains(r, x, y));
        self.anims.get(&format!("hover{key}"), on as u8 as f32, 120)
    }

    /// Rajzolás (x, y) eltolással, a görgetést is figyelembe véve.
    unsafe fn at(&self, x: f32, y: f32) {
        self.p.ctx.SetTransform(&Matrix3x2::translation(x, y - self.scroll));
    }

    unsafe fn reset(&self) {
        self.p.ctx.SetTransform(&Matrix3x2::translation(0.0, -self.scroll));
    }

    unsafe fn section(&self, y: &mut f32, title: &str) -> Result<()> {
        self.p.text(title, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(MARGIN, *y, 300.0, 18.0), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        *y += 24.0;
        Ok(())
    }

    /// Egy sor háttérrel, címmel és alcímmel; a vezérlő függőleges közepét adja vissza.
    unsafe fn row(&self, y: &mut f32, title: &str, subtitle: Option<&str>) -> Result<f32> {
        let h = if subtitle.is_some() { 56.0 } else { 48.0 };
        self.p.fill(rect(MARGIN, *y, W - 2.0 * MARGIN, h), 8.0, self.theme.row())?;
        let tw = 360.0;
        match subtitle {
            Some(sub) => {
                self.p.text(title, 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, *y + 9.0, tw, 20.0), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?;
                self.p.text(sub, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, *y + 29.0, tw, 18.0), self.theme.sub(), DWRITE_TEXT_ALIGNMENT_LEADING)?;
            }
            None => self.p.text(title, 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, *y, tw, h), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?,
        }
        let cy = *y + h / 2.0;
        *y += h + ROW_GAP;
        Ok(cy)
    }

    /// Kapcsoló a jobb szélen (40x20, a design szerint), 56x44-es kattintható területtel.
    /// A gomb átcsúszik, a sáv színe átúszik; rámutatáskor a gomb kicsit nagyobb.
    unsafe fn switch(&mut self, right: f32, cy: f32, on: bool, act: Act) -> Result<()> {
        let r = rect(right - 48.0, cy - 10.0, 40.0, 20.0);
        let hit = rect(right - 56.0, cy - 22.0, 56.0, 44.0);
        let key = format!("{act:?}");
        let v = self.anims.get(&format!("switch{key}"), on as u8 as f32, 160);
        let h = self.hover_anim(&key, &hit);
        if v < 1.0 {
            self.p.stroke(r, 10.0, 1.0, fade(self.theme.control_border(), 1.0 - v))?;
        }
        if v > 0.0 {
            self.p.fill(r, 10.0, fade(self.accent, v))?;
        }
        let knob = mix(self.theme.knob_off(), self.theme.on_accent(), v);
        self.p.circle(r.left + 10.0 + 20.0 * v, cy, 6.0 + h, knob)?;
        self.hits.push((hit, act));
        Ok(())
    }

    /// Szegmensválasztó a jobb szélen.
    unsafe fn segmented(&mut self, right: f32, cy: f32, options: &[(&'static str, &str)], selected: &str, act: fn(&'static str) -> Act) -> Result<()> {
        let widths: Vec<f32> = options.iter().map(|(_, label)| Ok::<f32, windows::core::Error>(self.p.text_width(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM)? + 28.0)).collect::<Result<_>>()?;
        let total = widths.iter().sum::<f32>() + 2.0 * (options.len() as f32 - 1.0) + 6.0;
        let mut x = right - 16.0 - total;
        self.p.fill(rect(x, cy - 20.0, total, 40.0), 8.0, self.theme.window())?;
        x += 3.0;
        let mut rects = Vec::new();
        for w in widths {
            rects.push(rect(x, cy - 17.0, w, 34.0));
            x += w + 2.0;
        }
        // A kijelölés átcsúszik az új opcióra (helye és szélessége is animál).
        let group = format!("{:?}", act(options[0].0));
        if let Some(r) = options.iter().position(|(k, _)| *k == selected).map(|i| rects[i]) {
            let sx = self.anims.get(&format!("seg-x{group}"), r.left, 200);
            let sw = self.anims.get(&format!("seg-w{group}"), r.right - r.left, 200);
            self.p.fill(rect(sx, r.top, sw, 34.0), 6.0, self.theme.segment_selected())?;
        }
        for (i, ((key, label), r)) in options.iter().zip(rects).enumerate() {
            let sel = *key == selected;
            let h = self.hover_anim(&format!("{group}{i}"), &r);
            if h > 0.0 && !sel {
                self.p.fill(r, 6.0, self.theme.fg(0.05 * h))?;
            }
            let t = self.anims.get(&format!("seg-t{group}{i}"), sel as u8 as f32, 200);
            let color = mix(self.theme.sub(), self.theme.fg(1.0), t);
            self.p.text(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM, r, color, DWRITE_TEXT_ALIGNMENT_CENTER)?;
            self.hits.push((r, act(key)));
        }
        Ok(())
    }

    unsafe fn radio(&mut self, x: f32, y: f32, label: &str, hint: &str, on: bool, act: Act) -> Result<()> {
        let (cx, cy) = (x + 9.0, y + 22.0);
        let hit = rect(x, y, 560.0, 44.0);
        let key = format!("{act:?}");
        let v = self.anims.get(&format!("radio{key}"), on as u8 as f32, 160);
        let h = self.hover_anim(&key, &hit);
        if h > 0.0 {
            self.p.circle(cx, cy, 14.0, self.theme.fg(0.06 * h))?;
        }
        let ring = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: 0.0, radiusY: 0.0 };
        let (width, color) = (2.0 + 3.0 * v, mix(self.theme.control_border(), self.accent, v));
        let e = D2D1_ELLIPSE { radiusX: 9.0 - width / 2.0, radiusY: 9.0 - width / 2.0, ..ring };
        self.p.ctx.DrawEllipse(&e, &self.p.brush(&color)?, width, None);
        self.p.text(label, 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 30.0, y + 4.0, 500.0, 20.0), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.p.text(hint, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 30.0, y + 23.0, 500.0, 18.0), self.theme.sub(), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.hits.push((hit, act));
        Ok(())
    }

    /// Gomb a jobb szélen: elsődleges (kiemelő színnel) vagy másodlagos (semleges).
    unsafe fn button(&mut self, right: f32, cy: f32, label: &str, primary: bool, act: Act) -> Result<()> {
        let w = self.p.text_width(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM)? + 28.0;
        let r = rect(right - 16.0 - w, cy - 16.0, w, 32.0);
        let (bg, fg) = if primary { (self.accent, self.theme.on_accent()) } else { (self.theme.segment_selected(), self.theme.fg(1.0)) };
        self.p.fill(r, 6.0, bg)?;
        let h = self.hover_anim(&format!("{act:?}"), &r);
        if h > 0.0 {
            self.p.fill(r, 6.0, self.theme.fg(0.08 * h))?;
        }
        self.p.text(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM, r, fg, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        self.hits.push((r, act));
        Ok(())
    }

    /// Egy eszköz sora a gyűrűs ikonjával, nevével és egy alsorral; a sor közepének y-ját adja.
    unsafe fn device_row(&mut self, y: &mut f32, d: &DeviceState, sub: &str) -> Result<f32> {
        self.p.fill(rect(MARGIN, *y, W - 2.0 * MARGIN, 52.0), 8.0, self.theme.row())?;
        self.at(MARGIN + 14.0, *y + 10.0);
        self.p.renderer.draw(self.p.ctx, d, 32.0, self.theme, Layout::Ring, Some(self.theme.row()))?;
        self.reset();
        self.p.text(&d.name, 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 60.0, *y + 8.0, 440.0, 20.0), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.p.text(sub, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 60.0, *y + 27.0, 440.0, 18.0), self.theme.sub(), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let cy = *y + 26.0;
        *y += 52.0 + ROW_GAP;
        Ok(cy)
    }

    /// A tálca eszközlistájának egy sora `y`-on: fogantyú, gyűrűs ikon, név, kapcsoló.
    /// A húzott (`lifted`) sor kiemelt háttérrel és árnyékkal "felemelkedik".
    unsafe fn list_row(&mut self, d: &DeviceState, y: f32, lifted: bool, s: &Settings) -> Result<()> {
        let l = self.anims.get(&format!("lift{}", d.id), lifted as u8 as f32, 140);
        let r = rect(MARGIN, y, W - 2.0 * MARGIN, 52.0);
        if l > 0.0 {
            for (grow, alpha) in [(8.0, 0.05), (4.0, 0.08), (1.0, 0.10)] {
                self.p.fill(rect(r.left - grow, y - grow + 4.0, r.right - r.left + 2.0 * grow, 52.0 + 2.0 * grow), 8.0 + grow, rgb(0, alpha * l))?;
            }
        }
        let bg = mix(self.theme.row(), self.theme.segment_selected(), l);
        self.p.fill(r, 8.0, bg)?;
        // Fogantyú (hat pötty): ennél fogva húzható a sor.
        let handle = rect(MARGIN, y, 30.0, 52.0);
        let h = self.hover_anim(&format!("handle{}", d.id), &handle).max(l);
        let dots = mix(self.theme.control_border(), self.theme.fg(1.0), h * 0.6);
        for (dx, dy) in [(0.0, 0.0), (6.0, 0.0), (0.0, 5.0), (6.0, 5.0), (0.0, 10.0), (6.0, 10.0)] {
            self.p.circle(MARGIN + 13.0 + dx, y + 21.0 + dy, 1.4, dots)?;
        }
        self.hits.push((handle, Act::Drag(d.id.clone())));
        self.at(MARGIN + 32.0, y + 10.0);
        self.p.renderer.draw(self.p.ctx, d, 32.0, self.theme, Layout::Ring, Some(bg))?;
        self.reset();
        let (fg, sub) = (self.theme.fg(1.0), self.theme.sub());
        self.p.text(&d.name, 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 78.0, y + 8.0, 420.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.p.text(device_conn(d), 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 78.0, y + 27.0, 420.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.switch(W - MARGIN, y + 26.0, !s.hidden.contains(&d.id), Act::Device(d.id.clone()))
    }

    /// Az Eszközök oldal: a NapCat eszközei (eltávolítás), és a Windows által észlelt, még nem
    /// megjelenített eszközök (hozzáadás).
    unsafe fn build_devices(&mut self, s: &Settings, devices: &[DeviceState], candidates: &[Candidate]) -> Result<f32> {
        let fg = self.theme.fg(1.0);
        let right = W - MARGIN;
        let mut y = 24.0;
        self.p.logo(MARGIN, y, 32.0)?;
        self.p.text(t("Eszközök", "Devices"), 24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(MARGIN + 44.0, y, 300.0, 32.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.button(right + 16.0, y + 16.0, t("‹ Beállítások", "‹ Settings"), false, Act::Page(Page::Settings))?;
        y += 32.0 + SECTION_GAP;

        // A NapCat-ben megjelenő eszközök.
        self.section(&mut y, t("A NapCat-ben", "In NapCat"))?;
        let active: Vec<&DeviceState> = devices.iter().filter(|d| !s.removed.contains(&d.id)).collect();
        if active.is_empty() {
            self.row(&mut y, t("Még nincs eszköz", "No devices yet"), None)?;
        }
        for d in active {
            let custom = s.custom.iter().any(|c| c.id() == d.id);
            let sub = match (custom, d.connected) {
                (true, true) => t("Kézzel hozzáadva · csatlakoztatva (akkuszint nem elérhető)", "Added manually · connected (battery level not available)"),
                (true, false) => t("Kézzel hozzáadva · nincs csatlakoztatva", "Added manually · not connected"),
                (false, _) => device_conn(d),
            };
            let cy = self.device_row(&mut y, d, sub)?;
            self.button(right, cy, t("Eltávolítás", "Remove"), false, Act::Remove(d.id.clone()))?;
            // Típus (glyph): automatikus, vagy a felhasználó választása; kattintásra körbejár.
            let remove_w = self.p.text_width(t("Eltávolítás", "Remove"), 12.5, DWRITE_FONT_WEIGHT_MEDIUM)? + 28.0;
            let label = match s.kind_override(&d.id) {
                Some(k) => kind_label(custom::kind_from_str(k)).to_string(),
                None => format!("{} (auto)", kind_label(d.kind)),
            };
            self.button(right - remove_w - 8.0, cy, &label, false, Act::Kind(d.id.clone()))?;
        }
        y += SECTION_GAP - ROW_GAP;

        // Hozzáadható: korábban eltávolított eszközök és a Windows által észlelt, ismeretlen eszközök.
        self.section(&mut y, t("Észlelt eszközök", "Detected devices"))?;
        let removed: Vec<&DeviceState> = devices.iter().filter(|d| s.removed.contains(&d.id)).collect();
        if removed.is_empty() && candidates.is_empty() {
            self.row(&mut y, t("Nincs több észlelt eszköz", "No other detected devices"), Some(t("Csatlakoztass egy eszközt, és itt megjelenik", "Connect a device and it shows up here")))?;
        }
        for d in removed {
            let cy = self.device_row(&mut y, d, t("Korábban eltávolítva", "Removed earlier"))?;
            self.button(right, cy, t("Hozzáadás", "Add"), true, Act::Restore(d.id.clone()))?;
        }
        for (i, c) in candidates.iter().enumerate() {
            let preview = DeviceState { connected: true, ..DeviceState::disconnected("", &c.product, c.kind) };
            let sub = format!("{} · {:04X}:{:04X} · {}", t("USB / vevő", "USB / receiver"), c.vid, c.pid, t("csak a csatlakozás látszik", "connection only"));
            let cy = self.device_row(&mut y, &preview, &sub)?;
            self.button(right, cy, t("Hozzáadás", "Add"), true, Act::Add(i))?;
        }
        y += SECTION_GAP - ROW_GAP;

        // Új Bluetooth eszköz párosítása a Windowsban (utána itt, vagy magától megjelenik).
        self.section(&mut y, "Bluetooth")?;
        let cy = self.row(&mut y, t("Új Bluetooth eszköz párosítása", "Pair a new Bluetooth device"), Some(t("A Windows Bluetooth beállításaiban; utána a NapCat magától felismeri", "In the Windows Bluetooth settings; NapCat picks it up afterwards")))?;
        self.button(right, cy, t("Párosítás", "Pair"), false, Act::PairBluetooth)?;
        Ok(y + 32.0 - ROW_GAP)
    }

    unsafe fn build(&mut self, s: &Settings, autostart: bool, devices: &[DeviceState]) -> Result<f32> {
        let (fg, sub) = (self.theme.fg(1.0), self.theme.sub());
        let right = W - MARGIN;
        let mut y = 24.0;
        self.p.logo(MARGIN, y, 32.0)?;
        self.p.text(t("Beállítások", "Settings"), 24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(MARGIN + 44.0, y, 400.0, 32.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        // Verzió a jobb felső sarokban; ha van frissítés, kiemelő színnel jelzi (a gomb a Névjegyben).
        let update = updater::AVAILABLE.lock().ok().and_then(|a| a.clone());
        let (version, color) = match &update {
            Some(r) => (format!("v{} · {}: v{}", env!("CARGO_PKG_VERSION"), t("frissítés", "update"), r.version), self.accent),
            None => (format!("v{}", env!("CARGO_PKG_VERSION")), sub),
        };
        self.p.text(&version, 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(right - 300.0, y + 8.0, 300.0, 18.0), color, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        y += 32.0 + SECTION_GAP;

        // Általános
        self.section(&mut y, t("Általános", "General"))?;
        let cy = self.row(&mut y, t("Indítás a Windows-zal", "Start with Windows"), Some(t("A tálcán, csendben indul", "Starts quietly in the tray")))?;
        self.switch(right, cy, autostart, Act::Startup)?;
        let cy = self.row(&mut y, t("Tálca mód", "Tray mode"), Some(t("Egy ikon a legsürgősebb eszközzel, vagy mindegyik külön", "One icon for the most urgent device, or one per device")))?;
        self.segmented(right, cy, &[("one", t("Egy ikon", "One icon")), ("each", t("Eszközönként", "Per device"))], &s.tray_mode, Act::TrayMode)?;
        let cy = self.row(&mut y, t("Ikon stílusa", "Icon style"), Some(t("Gyűrű, a szint száma, vagy nagy ikon jelvénnyel / sávval", "Ring, the level as a number, or a large icon with a badge or bar")))?;
        self.segmented(right, cy, &[("ring", t("Gyűrű", "Ring")), ("percent", t("Százalék", "Percent")), ("pie", t("Jelvény", "Badge")), ("bar", t("Sáv", "Bar"))], &s.icon_style, Act::IconStyle)?;
        let cy = self.row(&mut y, t("Téma", "Theme"), None)?;
        self.segmented(right, cy, &[("system", t("Rendszer", "System")), ("dark", t("Sötét", "Dark")), ("light", t("Világos", "Light"))], &s.theme, Act::Theme)?;
        let cy = self.row(&mut y, t("Nyelv", "Language"), None)?;
        self.segmented(right, cy, &[("en", "English"), ("hu", "Magyar"), ("auto", t("Rendszer", "System"))], &s.language, Act::Language)?;
        y += SECTION_GAP - ROW_GAP;

        // Kiemelő szín
        self.section(&mut y, t("Kiemelő szín", "Accent color"))?;
        let cy = self.row(&mut y, t("Kiemelő szín használata", "Use accent color"), Some(t("Kikapcsolva semleges, fehér-szürke megjelenés", "When off, a neutral white and gray look")))?;
        self.switch(right, cy, s.use_accent, Act::UseAccent)?;
        if s.use_accent {
            let custom = s.custom_accent.is_some();
            let box_h = 14.0 + 88.0 + if custom { 50.0 } else { 0.0 } + 14.0 + 56.0 + 14.0 + 48.0 + 6.0;
            self.p.fill(rect(MARGIN, y, W - 2.0 * MARGIN, box_h), 8.0, self.theme.row())?;
            let x = MARGIN + 16.0;
            let mut by = y + 14.0;
            self.radio(x, by, t("Windows kiemelő színe", "Windows accent color"), t("Automatikusan követi a Személyre szabás › Színek beállítást", "Follows Personalization › Colors automatically"), !custom, Act::Source(false))?;
            by += 44.0;
            self.radio(x, by, t("Egyéni szín", "Custom color"), t("Válassz a palettáról", "Pick one from the palette"), custom, Act::Source(true))?;
            by += 44.0;
            if custom {
                let mut sx = x + 30.0;
                for (_, hex) in SWATCHES {
                    let (cx, cy) = (sx + 22.0, by + 25.0);
                    let hit = rect(sx, by + 3.0, 44.0, 44.0);
                    // A kijelölő gyűrű kinyílik; rámutatáskor a minta kicsit nagyobb.
                    let v = self.anims.get(&format!("swatch{hex}"), (s.custom_accent == Some(hex)) as u8 as f32, 180);
                    let h = self.hover_anim(&format!("swatch{hex}"), &hit);
                    if v > 0.0 {
                        self.p.circle(cx, cy, 15.0 + 4.0 * v, fade(fg, v))?;
                        self.p.circle(cx, cy, 15.0 + 2.0 * v, self.theme.row())?;
                    }
                    self.p.circle(cx, cy, 15.0 + h * (1.0 - v), rgb(hex, 1.0))?;
                    self.hits.push((hit, Act::Swatch(hex)));
                    sx += 50.0;
                }
                by += 50.0;
            }
            by += 14.0;
            // Előnézet: gyűrű, kiemelt gomb, kapcsoló.
            self.p.fill(rect(x, by, W - 2.0 * MARGIN - 32.0, 56.0), 8.0, self.theme.window())?;
            self.p.text(t("Előnézet", "Preview"), 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 14.0, by, 70.0, 56.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            let sample = DeviceState {
                level: Some(78),
                charge: ChargeState::Discharging,
                connected: true,
                ..DeviceState::disconnected("preview", t("Előnézet", "Preview"), DeviceKind::Mouse)
            };
            self.at(x + 96.0, by + 12.0);
            self.p.renderer.draw(self.p.ctx, &sample, 32.0, self.theme, Layout::Ring, Some(self.theme.window()))?;
            self.reset();
            let label = t("Eszköz párosítása", "Pair a device");
            let bw = self.p.text_width(label, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)? + 24.0;
            self.p.fill(rect(x + 144.0, by + 12.0, bw, 32.0), 6.0, self.accent)?;
            self.p.text(label, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x + 144.0, by + 12.0, bw, 32.0), self.theme.on_accent(), DWRITE_TEXT_ALIGNMENT_CENTER)?;
            let sw = rect(x + 144.0 + bw + 16.0, by + 18.0, 40.0, 20.0);
            self.p.fill(sw, 10.0, self.accent)?;
            self.p.circle(sw.right - 10.0, by + 28.0, 6.0, self.theme.on_accent())?;
            by += 56.0 + 14.0;
            self.p.text(t("Rendben állapot a kiemelő színnel", "Accent color for healthy levels"), 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(x, by, 420.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.p.text(t("50% felett a gyűrű zöld helyett ezt a színt kapja", "Above 50% the ring uses this color instead of green"), 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x, by + 20.0, 420.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.switch(right, by + 19.0, s.ring_accent, Act::RingAccent)?;
            y += box_h + ROW_GAP;
        }
        y += SECTION_GAP - ROW_GAP;

        // Eszközök a tálcán
        self.section(&mut y, t("Eszközök a tálcán", "Devices in the tray"))?;
        let shown: Vec<&DeviceState> = devices.iter().filter(|d| !s.removed.contains(&d.id)).collect();
        if shown.is_empty() {
            self.row(&mut y, t("Még nincs ismert eszköz", "No known devices yet"), None)?;
        }
        // Húzás közben a húzott sor a kurzort követi, a többi félrecsúszik a helye elől;
        // elengedéskor a húzott sor a helyére úszik.
        let list_top = y;
        let drag = self.drag.clone();
        let target = drag.as_ref().map(|(_, cy, off)| drag_index(list_top, shown.len(), cy - off));
        let mut slot = 0;
        let mut lifted = None;
        for d in &shown {
            if let Some((_, cy, off)) = drag.as_ref().filter(|(id, _, _)| *id == d.id) {
                lifted = Some((*d, cy - off));
                continue;
            }
            if Some(slot) == target {
                slot += 1;
            }
            let ry = self.anims.get(&format!("row{}", d.id), list_top + slot as f32 * PITCH, 180);
            self.list_row(d, ry, false, s)?;
            slot += 1;
        }
        if let Some((d, ry)) = lifted {
            self.anims.set(&format!("row{}", d.id), ry);
            self.list_row(d, ry, true, s)?;
        }
        y = list_top + shown.len() as f32 * PITCH;
        self.list = (list_top, shown.iter().map(|d| d.id.clone()).collect());
        let cy = self.row(&mut y, t("Eszközök hozzáadása és eltávolítása", "Add and remove devices"), Some(t("A Windows által észlelt eszközök listája", "The devices Windows has detected")))?;
        self.button(right, cy, t("Eszközök kezelése", "Manage devices"), false, Act::Page(Page::Devices))?;
        y += SECTION_GAP - ROW_GAP;

        // Értesítések
        self.section(&mut y, t("Értesítések", "Notifications"))?;
        self.p.fill(rect(MARGIN, y, W - 2.0 * MARGIN, 116.0), 8.0, self.theme.row())?;
        self.p.text(t("Lemerülés figyelmeztetés", "Low battery warning"), 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, y + 9.0, 420.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let msg = if crate::i18n::english() {
            format!("Warns when a device drops below {}%", s.low_threshold)
        } else {
            format!("Szól, ha egy eszköz {}% alá esik", s.low_threshold)
        };
        self.p.text(&msg, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, y + 29.0, 420.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.switch(right, y + 28.0, s.notify_low, Act::NotifyLow)?;
        // Küszöb csúszka.
        let sy = y + 74.0;
        self.p.text(t("Küszöb", "Threshold"), 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, sy - 10.0, 60.0, 20.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let track = rect(MARGIN + 90.0, sy - 2.0, W - 2.0 * MARGIN - 90.0 - 70.0, 4.0);
        // Húzás közben a gomb pontosan a kurzort követi; elengedéskor a legközelebbi 5%-os
        // lépésre ugrik (animálva). A lépések pontként és felirattal látszanak.
        let snapped = (s.low_threshold as f32 - 5.0) / 25.0;
        let pos = match self.slider {
            Some(raw) => {
                self.anims.set("threshold", raw);
                raw
            }
            None => self.anims.get("threshold", snapped, 180),
        };
        let tx = track.left + pos * (track.right - track.left);
        self.p.fill(track, 2.0, self.theme.control_border())?;
        self.p.fill(rect(track.left, track.top, tx - track.left, 4.0), 2.0, self.accent)?;
        for i in 0..=5 {
            let k = i as f32 / 5.0;
            let x = track.left + k * (track.right - track.left);
            let passed = k <= pos + 0.001;
            self.p.circle(x, sy, 3.5, if passed { self.accent } else { self.theme.sub() })?;
            let current = (k - snapped).abs() < 0.01;
            let (weight, color) = if current { (DWRITE_FONT_WEIGHT_SEMI_BOLD, fg) } else { (DWRITE_FONT_WEIGHT_NORMAL, sub) };
            self.p.text(&format!("{}%", 5 + i * 5), 11.0, weight, rect(x - 20.0, sy + 11.0, 40.0, 16.0), color, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        }
        let grow = self.anims.get("threshold-grab", self.slider.is_some() as u8 as f32, 120);
        self.p.circle(tx, sy, 9.0 + 2.0 * grow, self.accent)?;
        self.p.circle(tx, sy, 4.0, self.theme.on_accent())?;
        self.p.text(&format!("{}%", s.low_threshold), 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(right - 56.0, sy - 10.0, 40.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        self.hits.push((rect(track.left - 10.0, sy - 14.0, track.right - track.left + 20.0, 44.0), Act::Threshold));
        y += 116.0 + ROW_GAP;
        let cy = self.row(&mut y, t("Teli töltés értesítés", "Fully charged notification"), None)?;
        self.switch(right, cy, s.notify_full, Act::NotifyFull)?;
        let cy = self.row(&mut y, t("Csendes mód teljes képernyőn", "Quiet in full screen"), Some(t("Játék vagy film közben az értesítések várnak", "Notifications wait while a game or film is in full screen")))?;
        self.switch(right, cy, s.quiet_fullscreen, Act::Quiet)?;
        y += SECTION_GAP - ROW_GAP;

        // Teljesítmény
        self.section(&mut y, t("Teljesítmény", "Performance"))?;
        let cy = self.row(&mut y, t("Frissítési gyakoriság", "Refresh rate"), Some(t("Ritkább frissítés = kevesebb erőforrás", "Less often = fewer resources")))?;
        let poll = match s.poll_secs {
            ..=30 => "30",
            31..=60 => "60",
            _ => "300",
        };
        self.segmented(right, cy, &[("30", t("30 másodperc", "30 seconds")), ("60", t("1 perc", "1 minute")), ("300", t("5 perc", "5 minutes"))], poll, |k| Act::Poll(k.parse().unwrap_or(60)))?;
        let cy = self.row(&mut y, t("Frissítések", "Updates"), Some(t("Új NapCat verzió a GitHubról", "New NapCat versions from GitHub")))?;
        self.segmented(right, cy, &[("auto", t("Automatikus", "Automatic")), ("notify", t("Értesítés", "Notify")), ("off", t("Ki", "Off"))], &s.update_mode, Act::Updates)?;
        y += SECTION_GAP - ROW_GAP;

        // Névjegy: verzió és frissítés
        self.section(&mut y, t("Névjegy", "About"))?;
        let available = updater::AVAILABLE.lock().ok().and_then(|a| a.clone());
        let installing = tray::INSTALLING.load(Ordering::Relaxed);
        let checking = updater::CHECKING.load(Ordering::Relaxed);
        let status = match (&available, installing, checking) {
            (Some(r), true, _) => format!("{} {}…", t("Letöltés és telepítés:", "Downloading and installing"), r.version),
            (Some(r), false, _) => format!("{}: {}", t("Új verzió érhető el", "New version available"), r.version),
            (None, _, true) => t("Frissítés keresése…", "Checking for updates…").to_string(),
            (None, _, false) => match updater::LAST_CHECK.load(Ordering::Relaxed) {
                1 => t("Ez a legfrissebb verzió", "You're on the latest version").to_string(),
                2 => t("A frissítés keresése nem sikerült", "Couldn't check for updates").to_string(),
                _ => t("Frissítések a GitHubról", "Updates from GitHub").to_string(),
            },
        };
        let title = format!("NapCat {}", env!("CARGO_PKG_VERSION"));
        let cy = self.row(&mut y, &title, Some(&status))?;
        match (&available, installing || checking) {
            (Some(_), false) => self.button(right, cy, t("Telepítés", "Install"), true, Act::InstallUpdate)?,
            (None, false) => self.button(right, cy, t("Frissítés keresése", "Check for updates"), false, Act::CheckUpdate)?,
            (_, true) => {}
        }
        Ok(y + 32.0 - ROW_GAP)
    }
}

unsafe fn paint(w: &mut Window) -> Result<()> {
    let mut rc = RECT::default();
    GetClientRect(w.hwnd, &mut rc)?;
    let size = D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 };
    if w.rt.is_none() {
        // Szoftveres megjelenítés: a kis felülethez nem kell GPU, és így nem töltődik be a videókártya-
        // meghajtó (Direct3D), ami bezárás után is ~20 MB-ot foglalna.
        let props = D2D1_RENDER_TARGET_PROPERTIES { r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, dpiX: w.dpi, dpiY: w.dpi, ..Default::default() };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd: w.hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_NONE };
        w.rt = Some(w.renderer.d2d().CreateHwndRenderTarget(&props, &hwnd_props)?);
    }
    let rt = w.rt.as_ref().unwrap();
    rt.Resize(&size)?;
    rt.SetDpi(w.dpi, w.dpi);
    let ctx: ID2D1DeviceContext = rt.cast()?;
    let theme = Theme::ui();
    // Címsor és tálcagomb ikon: téma vagy "feltöltve" állapot változásakor újra.
    let style = AppIcon::current(theme.light);
    if w.icon_style != Some(style) {
        set_window_icons(w.hwnd, w.dpi, style);
        w.icon_style = Some(style);
    }
    ctx.BeginDraw();
    ctx.Clear(Some(&theme.window()));
    // A görgetést egy eltolással kezeljük; a rajzoló segédek a jelenlegi transzformációra rajzolnak.
    let mut anims = std::mem::take(&mut w.anims);
    anims.frame();
    // Oldalváltáskor a tartalom alulról beúszik és előtűnik.
    let page = anims.get("page", 1.0, 220);
    let mut ui = Ui {
        p: Painter { ctx: &ctx, renderer: &w.renderer },
        theme,
        accent: theme.accent(),
        hits: Vec::new(),
        scroll: w.scroll - (1.0 - page) * 16.0,
        drag: w.drag_device.clone(),
        list: (0.0, Vec::new()),
        anims,
        hover: w.hover.map(|(x, y)| (x, y + w.scroll)),
        slider: w.slider,
    };
    ui.reset();
    let result = match w.page {
        Page::Settings => ui.build(&w.settings, w.autostart, &w.devices),
        Page::Devices => ui.build_devices(&w.settings, &w.devices, &w.candidates),
    };
    w.hits = std::mem::take(&mut ui.hits);
    w.list = std::mem::take(&mut ui.list);
    w.anims = std::mem::take(&mut ui.anims);
    if let Ok(h) = result {
        w.content_h = h;
        // Átméretezés vagy rövidebb oldal után a görgetés ne lógjon túl a tartalmon.
        w.scroll = w.scroll.clamp(0.0, max_scroll(w));
    }
    ctx.SetTransform(&Matrix3x2::identity());
    if page < 1.0 {
        let _ = ui.p.fill(rect(0.0, 0.0, W, client_h(w)), 0.0, fade(theme.window(), 1.0 - page));
    }
    // Görgetősáv (Windows 11 stílus): vékony csík, rámutatáskor és húzás közben szélesebb, sávval.
    if let Some((track, thumb)) = scrollbar(w) {
        let wide = w.anims.get("sb", (w.sb_hover || w.sb_drag.is_some()) as u8 as f32, 120);
        let width = 3.0 + 3.0 * wide;
        let x = track.right - 3.0 - width;
        if wide > 0.0 {
            let _ = ui.p.fill(rect(track.left, track.top, track.right - track.left, track.bottom - track.top), 6.0, theme.fg(0.06 * wide));
        }
        let alpha = if w.sb_drag.is_some() { 0.6 } else { 0.35 + 0.15 * wide };
        let _ = ui.p.fill(rect(x, thumb.top, width, thumb.bottom - thumb.top), width / 2.0, theme.fg(alpha));
    }
    // Amíg valami mozog, a következő képkocka; utána az időzítő leáll.
    if w.anims.active() {
        SetTimer(w.hwnd, ANIM_TIMER, anim::FRAME_MS, None);
    } else {
        let _ = KillTimer(w.hwnd, ANIM_TIMER);
    }
    if let Err(e) = ctx.EndDraw(None, None) {
        if e.code() == D2DERR_RECREATE_TARGET {
            w.rt = None;
        }
    }
    Ok(())
}

/// Egy oldal teljes tartalma a képernyőképekhez (ugyanaz a rajzoló, mint az ablaké, görgetés és
/// rámutatás nélkül); a tartalom magasságát adja vissza DIP-ben.
#[cfg(test)]
pub unsafe fn gallery_draw(
    ctx: &ID2D1DeviceContext,
    renderer: &IconRenderer,
    page: Page,
    s: &Settings,
    devices: &[DeviceState],
    candidates: &[Candidate],
    theme: Theme,
) -> Result<f32> {
    ctx.Clear(Some(&theme.window()));
    let mut ui = Ui {
        p: Painter { ctx, renderer },
        theme,
        accent: theme.accent(),
        hits: Vec::new(),
        scroll: 0.0,
        drag: None,
        list: (0.0, Vec::new()),
        anims: Anims::default(),
        hover: None,
        slider: None,
    };
    ui.reset();
    let h = match page {
        Page::Settings => ui.build(s, true, devices),
        Page::Devices => ui.build_devices(s, devices, candidates),
    };
    ctx.SetTransform(&Matrix3x2::identity());
    h
}

#[cfg(test)]
pub const GALLERY_W: f32 = W;

fn client_h(w: &Window) -> f32 {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(w.hwnd, &mut rc);
    }
    (rc.bottom - rc.top) as f32 * 96.0 / w.dpi
}

fn client_w(w: &Window) -> f32 {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(w.hwnd, &mut rc);
    }
    (rc.right - rc.left) as f32 * 96.0 / w.dpi
}

fn max_scroll(w: &Window) -> f32 {
    (w.content_h - client_h(w)).max(0.0)
}

/// A görgetősáv kattintható sávja és a fogantyú (kliens DIP); None, ha a tartalom kifér.
fn scrollbar(w: &Window) -> Option<(D2D_RECT_F, D2D_RECT_F)> {
    let (cw, ch) = (client_w(w), client_h(w));
    let max = max_scroll(w);
    if max < 1.0 {
        return None;
    }
    let track = rect(cw - 14.0, 4.0, 12.0, ch - 8.0);
    let track_h = track.bottom - track.top;
    let thumb_h = (track_h * ch / w.content_h).clamp(32.0, track_h);
    let top = track.top + (w.scroll / max).clamp(0.0, 1.0) * (track_h - thumb_h);
    Some((track, rect(track.left, top, 12.0, thumb_h)))
}

/// Kurzorpozíció (lParam) kliens DIP-ben, görgetés nélkül.
fn to_client(w: &Window, lparam: LPARAM) -> (f32, f32) {
    let (x, y) = ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32);
    let k = 96.0 / w.dpi;
    (x * k, y * k)
}

/// Kurzorpozíció (lParam, kliens pixel) tartalom koordinátákban.
fn to_content(w: &Window, lparam: LPARAM) -> (f32, f32) {
    let (x, y) = ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32);
    let k = 96.0 / w.dpi;
    (x * k, y * k + w.scroll)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                WINDOW.with_borrow_mut(|w| {
                    if let Some(w) = w {
                        let _ = paint(w);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                WINDOW.with_borrow_mut(|w| {
                    if let Some(w) = w {
                        let max = max_scroll(w);
                        w.scroll = (w.scroll - delta * 64.0).clamp(0.0, max);
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                WINDOW.with_borrow_mut(|w| {
                    let Some(w) = w else { return };
                    // Görgetősáv: a fogantyú húzható, a sáv többi része laponként lapoz.
                    let (cx, cy) = to_client(w, lparam);
                    if let Some((track, thumb)) = scrollbar(w) {
                        if contains(&track, cx, cy) {
                            if cy >= thumb.top && cy <= thumb.bottom {
                                w.sb_drag = Some((cy, w.scroll));
                                SetCapture(hwnd);
                            } else {
                                let page = client_h(w) * 0.9;
                                let step = if cy < thumb.top { -page } else { page };
                                w.scroll = (w.scroll + step).clamp(0.0, max_scroll(w));
                            }
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                    }
                    let (x, y) = to_content(w, lparam);
                    match w.hits.iter().find(|(r, _)| contains(r, x, y)).cloned() {
                        Some((r, Act::Threshold)) => {
                            let track = rect(r.left + 10.0, r.top, r.right - r.left - 20.0, r.bottom - r.top);
                            w.dragging = Some(track);
                            SetCapture(hwnd);
                            drag_threshold(w, x, track);
                        }
                        Some((r, Act::Drag(id))) => {
                            w.drag_device = Some((id, y, y - r.top));
                            SetCapture(hwnd);
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                        _ => {}
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                WINDOW.with_borrow_mut(|w| {
                    let Some(w) = w else { return };
                    let (x, y) = to_content(w, lparam);
                    if !w.tracking {
                        let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                        w.tracking = TrackMouseEvent(&mut tme).is_ok();
                    }
                    let (cx, cy) = to_client(w, lparam);
                    if let Some((y0, scroll0)) = w.sb_drag {
                        // A fogantyú a kurzorral mozog; a tartalom arányosan görget.
                        if let Some((track, thumb)) = scrollbar(w) {
                            let room = (track.bottom - track.top) - (thumb.bottom - thumb.top);
                            if room > 0.0 {
                                w.scroll = (scroll0 + (cy - y0) * max_scroll(w) / room).clamp(0.0, max_scroll(w));
                            }
                        }
                        SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default());
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                    let on_bar = scrollbar(w).is_some_and(|(track, _)| contains(&track, cx, cy));
                    if on_bar != w.sb_hover {
                        w.sb_hover = on_bar;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    if on_bar {
                        w.hover = None;
                        SetCursor(LoadCursorW(None, IDC_ARROW).unwrap_or_default());
                        return;
                    }
                    w.hover = Some((x, y - w.scroll));
                    let hit = w.hits.iter().position(|(r, _)| contains(r, x, y));
                    if hit != w.hover_hit {
                        w.hover_hit = hit;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    if let Some(track) = w.dragging {
                        drag_threshold(w, x, track);
                    }
                    if let Some((_, drag_y, _)) = w.drag_device.as_mut() {
                        *drag_y = y;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    let on_handle = w.hits.iter().any(|(r, a)| matches!(a, Act::Drag(_)) && contains(r, x, y));
                    let cursor = if w.drag_device.is_some() || on_handle {
                        IDC_SIZENS
                    } else if w.dragging.is_some() || w.hits.iter().any(|(r, _)| contains(r, x, y)) {
                        IDC_HAND
                    } else {
                        IDC_ARROW
                    };
                    SetCursor(LoadCursorW(None, cursor).unwrap_or_default());
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                WINDOW.with_borrow_mut(|w| {
                    let Some(w) = w else { return };
                    if w.sb_drag.take().is_some() {
                        let _ = ReleaseCapture();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                    if w.dragging.take().is_some() {
                        // Elengedés: a gomb a legközelebbi lépésre úszik.
                        w.slider = None;
                        let _ = InvalidateRect(hwnd, None, false);
                        let _ = ReleaseCapture();
                        return;
                    }
                    if let Some((id, drag_y, offset)) = w.drag_device.take() {
                        let _ = ReleaseCapture();
                        drop_device(w, &id, drag_y - offset);
                        return;
                    }
                    let (x, y) = to_content(w, lparam);
                    if let Some((_, act)) = w.hits.iter().find(|(r, _)| contains(r, x, y)).cloned() {
                        on_act(w, act);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                WINDOW.with_borrow_mut(|w| {
                    if let Some(w) = w {
                        w.tracking = false;
                        w.sb_hover = false;
                        w.hover = None;
                        w.hover_hit = None;
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                });
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == ANIM_TIMER => {
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lparam.0 as *const RECT);
                WINDOW.with_borrow_mut(|w| {
                    if let Some(w) = w {
                        w.dpi = (wparam.0 & 0xFFFF) as f32;
                        w.icon_style = None;
                    }
                });
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                apply_frame(hwnd);
                let _ = InvalidateRect(hwnd, None, false);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                for which in [ICON_SMALL, ICON_BIG] {
                    let icon = SendMessageW(hwnd, WM_GETICON, WPARAM(which as usize), LPARAM(0));
                    if icon.0 != 0 {
                        let _ = DestroyIcon(HICON(icon.0 as *mut c_void));
                    }
                }
                WINDOW.set(None);
                let _ = K32EmptyWorkingSet(GetCurrentProcess());
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
