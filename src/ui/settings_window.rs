//! Beállítások ablak a design "Beállítások" artboardja szerint: Általános, Kiemelő szín, Eszközök a
//! tálcán, Értesítések, Teljesítmény. Direct2D-vel rajzolt vezérlők (kapcsoló, szegmensválasztó,
//! rádiógomb, színminta, csúszka); minden változás azonnal mentődik, és a tálca ablak alkalmazza.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;

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
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::draw::{contains, rect, Painter};
use super::icon_render::{IconRenderer, Layout};
use super::theme::{self, rgb, Theme};
use crate::model::{ChargeState, DeviceKind, DeviceState};
use crate::settings::{self, Settings};

/// Üzenet a tálca ablaknak: a beállítások megváltoztak (újraolvasás a fájlból).
pub const WM_SETTINGS_CHANGED: u32 = WM_APP + 6;

const W: f32 = 720.0;
const MARGIN: f32 = 32.0;
const SECTION_GAP: f32 = 26.0;
const ROW_GAP: f32 = 4.0;

const SWATCHES: [(&str, u32); 7] = [
    ("Égkék", 0x4CC2FF),
    ("Levendula", 0xA78BFA),
    ("Rózsaszín", 0xF472B6),
    ("Narancs", 0xFB923C),
    ("Citrom", 0xFACC15),
    ("Menta", 0x34D399),
    ("Türkiz", 0x2DD4BF),
];

#[derive(Clone, PartialEq)]
enum Act {
    Startup,
    TrayMode(&'static str),
    Theme(&'static str),
    IconStyle(&'static str),
    Updates(&'static str),
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
}

struct Window {
    hwnd: HWND,
    owner: HWND,
    settings: Settings,
    autostart: bool,
    devices: Vec<DeviceState>,
    renderer: IconRenderer,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: f32,
    scroll: f32,
    content_h: f32,
    hits: Vec<(D2D_RECT_F, Act)>,
    /// A küszöb csúszka húzása közben a csúszka sávja (tartalom koordinátákban).
    dragging: Option<D2D_RECT_F>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Megnyitja (vagy előtérbe hozza) a beállítások ablakot.
pub unsafe fn open(owner: HWND, devices: Vec<DeviceState>) {
    if let Some(hwnd) = WINDOW.with_borrow(|w| w.as_ref().map(|w| w.hwnd)) {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
        return;
    }
    let _ = create(owner, devices);
}

/// Friss eszközlista egy nyitott ablaknak.
pub unsafe fn update_devices(devices: &[DeviceState]) {
    WINDOW.with_borrow_mut(|w| {
        if let Some(w) = w {
            w.devices = devices.to_vec();
            let _ = InvalidateRect(w.hwnd, None, false);
        }
    });
}

const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);

unsafe fn create(owner: HWND, devices: Vec<DeviceState>) -> Result<()> {
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
    let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("NapCat – Beállítások"), STYLE, CW_USEDEFAULT, CW_USEDEFAULT, 100, 100, None, None, hinst, None)?;
    let dpi = GetDpiForWindow(hwnd) as f32;
    let settings = Settings::load();
    WINDOW.set(Some(Window {
        hwnd,
        owner,
        settings,
        autostart: settings::autostart_enabled(),
        devices,
        renderer: IconRenderer::new()?,
        rt: None,
        dpi,
        scroll: 0.0,
        content_h: 1180.0,
        hits: Vec::new(),
        dragging: None,
    }));
    apply_frame(hwnd);
    place(hwnd, dpi);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    Ok(())
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
    }
    changed(w);
}

/// Csúszka érték a vízszintes pozícióból (5-30%, 5-ös lépés).
unsafe fn drag_threshold(w: &mut Window, x: f32, track: D2D_RECT_F) {
    let t = ((x - track.left) / (track.right - track.left)).clamp(0.0, 1.0);
    let value = (5.0 + (t * 5.0).round() * 5.0) as u8;
    if w.settings.low_threshold != value {
        w.settings.low_threshold = value;
        changed(w);
    }
}

fn device_conn(d: &DeviceState) -> &'static str {
    match (d.connected, d.charge) {
        (false, _) => "Nincs csatlakoztatva",
        (true, ChargeState::Charging) => "Csatlakoztatva · tölt",
        (true, ChargeState::Full) => "Csatlakoztatva · feltöltve",
        _ => "Csatlakoztatva",
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
}

impl Ui<'_> {
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
    unsafe fn switch(&mut self, right: f32, cy: f32, on: bool, act: Act) -> Result<()> {
        let r = rect(right - 48.0, cy - 10.0, 40.0, 20.0);
        if on {
            self.p.fill(r, 10.0, self.accent)?;
            self.p.circle(r.right - 10.0, cy, 6.0, self.theme.on_accent())?;
        } else {
            self.p.stroke(r, 10.0, 1.0, self.theme.control_border())?;
            self.p.circle(r.left + 10.0, cy, 6.0, self.theme.knob_off())?;
        }
        self.hits.push((rect(right - 56.0, cy - 22.0, 56.0, 44.0), act));
        Ok(())
    }

    /// Szegmensválasztó a jobb szélen.
    unsafe fn segmented(&mut self, right: f32, cy: f32, options: &[(&'static str, &str)], selected: &str, act: fn(&'static str) -> Act) -> Result<()> {
        let widths: Vec<f32> = options.iter().map(|(_, label)| Ok::<f32, windows::core::Error>(self.p.text_width(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM)? + 28.0)).collect::<Result<_>>()?;
        let total = widths.iter().sum::<f32>() + 2.0 * (options.len() as f32 - 1.0) + 6.0;
        let mut x = right - 16.0 - total;
        self.p.fill(rect(x, cy - 20.0, total, 40.0), 8.0, self.theme.window())?;
        x += 3.0;
        for ((key, label), w) in options.iter().zip(widths) {
            let r = rect(x, cy - 17.0, w, 34.0);
            let sel = *key == selected;
            if sel {
                self.p.fill(r, 6.0, self.theme.segment_selected())?;
            }
            let color = if sel { self.theme.fg(1.0) } else { self.theme.sub() };
            self.p.text(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM, r, color, DWRITE_TEXT_ALIGNMENT_CENTER)?;
            self.hits.push((r, act(key)));
            x += w + 2.0;
        }
        Ok(())
    }

    unsafe fn radio(&mut self, x: f32, y: f32, label: &str, hint: &str, on: bool, act: Act) -> Result<()> {
        let (cx, cy) = (x + 9.0, y + 22.0);
        let ring = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: 0.0, radiusY: 0.0 };
        let (width, color) = if on { (5.0, self.accent) } else { (2.0, self.theme.control_border()) };
        let e = D2D1_ELLIPSE { radiusX: 9.0 - width / 2.0, radiusY: 9.0 - width / 2.0, ..ring };
        self.p.ctx.DrawEllipse(&e, &self.p.brush(&color)?, width, None);
        self.p.text(label, 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 30.0, y + 4.0, 500.0, 20.0), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.p.text(hint, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 30.0, y + 23.0, 500.0, 18.0), self.theme.sub(), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.hits.push((rect(x, y, 560.0, 44.0), act));
        Ok(())
    }

    unsafe fn build(&mut self, s: &Settings, autostart: bool, devices: &[DeviceState]) -> Result<f32> {
        let (fg, sub) = (self.theme.fg(1.0), self.theme.sub());
        let right = W - MARGIN;
        let mut y = 24.0;
        self.p.text("Beállítások", 24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(MARGIN, y, 400.0, 32.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        y += 32.0 + SECTION_GAP;

        // Általános
        self.section(&mut y, "Általános")?;
        let cy = self.row(&mut y, "Indítás a Windows-zal", Some("A tálcán, csendben indul"))?;
        self.switch(right, cy, autostart, Act::Startup)?;
        let cy = self.row(&mut y, "Tálca mód", Some("Egy ikon a legsürgősebb eszközzel, vagy mindegyik külön"))?;
        self.segmented(right, cy, &[("one", "Egy ikon"), ("each", "Eszközönként")], &s.tray_mode, Act::TrayMode)?;
        let cy = self.row(&mut y, "Ikon stílusa", Some("Gyűrű, vagy nagy ikon jelvénnyel / sávval"))?;
        self.segmented(right, cy, &[("ring", "Gyűrű"), ("pie", "Jelvény"), ("bar", "Sáv")], &s.icon_style, Act::IconStyle)?;
        let cy = self.row(&mut y, "Téma", None)?;
        self.segmented(right, cy, &[("system", "Rendszer"), ("dark", "Sötét"), ("light", "Világos")], &s.theme, Act::Theme)?;
        y += SECTION_GAP - ROW_GAP;

        // Kiemelő szín
        self.section(&mut y, "Kiemelő szín")?;
        let cy = self.row(&mut y, "Kiemelő szín használata", Some("Kikapcsolva semleges, fehér-szürke megjelenés"))?;
        self.switch(right, cy, s.use_accent, Act::UseAccent)?;
        if s.use_accent {
            let custom = s.custom_accent.is_some();
            let box_h = 14.0 + 88.0 + if custom { 50.0 } else { 0.0 } + 14.0 + 56.0 + 14.0 + 48.0 + 6.0;
            self.p.fill(rect(MARGIN, y, W - 2.0 * MARGIN, box_h), 8.0, self.theme.row())?;
            let x = MARGIN + 16.0;
            let mut by = y + 14.0;
            self.radio(x, by, "Windows kiemelő színe", "Automatikusan követi a Személyre szabás › Színek beállítást", !custom, Act::Source(false))?;
            by += 44.0;
            self.radio(x, by, "Egyéni szín", "Válassz a palettáról", custom, Act::Source(true))?;
            by += 44.0;
            if custom {
                let mut sx = x + 30.0;
                for (_, hex) in SWATCHES {
                    let (cx, cy) = (sx + 22.0, by + 25.0);
                    if s.custom_accent == Some(hex) {
                        self.p.circle(cx, cy, 19.0, fg)?;
                        self.p.circle(cx, cy, 17.0, self.theme.row())?;
                    }
                    self.p.circle(cx, cy, 15.0, rgb(hex, 1.0))?;
                    self.hits.push((rect(sx, by + 3.0, 44.0, 44.0), Act::Swatch(hex)));
                    sx += 50.0;
                }
                by += 50.0;
            }
            by += 14.0;
            // Előnézet: gyűrű, kiemelt gomb, kapcsoló.
            self.p.fill(rect(x, by, W - 2.0 * MARGIN - 32.0, 56.0), 8.0, self.theme.window())?;
            self.p.text("Előnézet", 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 14.0, by, 70.0, 56.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            let sample = DeviceState {
                level: Some(78),
                charge: ChargeState::Discharging,
                connected: true,
                ..DeviceState::disconnected("preview", "Előnézet", DeviceKind::Mouse)
            };
            self.at(x + 96.0, by + 12.0);
            self.p.renderer.draw(self.p.ctx, &sample, 32.0, self.theme, Layout::Ring, Some(self.theme.window()))?;
            self.reset();
            let label = "Eszköz párosítása";
            let bw = self.p.text_width(label, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)? + 24.0;
            self.p.fill(rect(x + 144.0, by + 12.0, bw, 32.0), 6.0, self.accent)?;
            self.p.text(label, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x + 144.0, by + 12.0, bw, 32.0), self.theme.on_accent(), DWRITE_TEXT_ALIGNMENT_CENTER)?;
            let sw = rect(x + 144.0 + bw + 16.0, by + 18.0, 40.0, 20.0);
            self.p.fill(sw, 10.0, self.accent)?;
            self.p.circle(sw.right - 10.0, by + 28.0, 6.0, self.theme.on_accent())?;
            by += 56.0 + 14.0;
            self.p.text("Rendben állapot a kiemelő színnel", 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(x, by, 420.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.p.text("50% felett a gyűrű zöld helyett ezt a színt kapja", 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x, by + 20.0, 420.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.switch(right, by + 19.0, s.ring_accent, Act::RingAccent)?;
            y += box_h + ROW_GAP;
        }
        y += SECTION_GAP - ROW_GAP;

        // Eszközök a tálcán
        self.section(&mut y, "Eszközök a tálcán")?;
        if devices.is_empty() {
            self.row(&mut y, "Még nincs ismert eszköz", None)?;
        }
        for d in devices {
            self.p.fill(rect(MARGIN, y, W - 2.0 * MARGIN, 52.0), 8.0, self.theme.row())?;
            self.at(MARGIN + 14.0, y + 10.0);
            self.p.renderer.draw(self.p.ctx, d, 32.0, self.theme, Layout::Ring, Some(self.theme.row()))?;
            self.reset();
            self.p.text(&d.name, 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 60.0, y + 8.0, 440.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.p.text(device_conn(d), 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 60.0, y + 27.0, 440.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.switch(right, y + 26.0, !s.hidden.contains(&d.id), Act::Device(d.id.clone()))?;
            y += 52.0 + ROW_GAP;
        }
        y += SECTION_GAP - ROW_GAP;

        // Értesítések
        self.section(&mut y, "Értesítések")?;
        self.p.fill(rect(MARGIN, y, W - 2.0 * MARGIN, 96.0), 8.0, self.theme.row())?;
        self.p.text("Lemerülés figyelmeztetés", 13.5, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, y + 9.0, 420.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let msg = format!("Szól, ha egy eszköz {}% alá esik", s.low_threshold);
        self.p.text(&msg, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, y + 29.0, 420.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.switch(right, y + 28.0, s.notify_low, Act::NotifyLow)?;
        // Küszöb csúszka.
        let sy = y + 74.0;
        self.p.text("Küszöb", 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(MARGIN + 16.0, sy - 10.0, 60.0, 20.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let track = rect(MARGIN + 90.0, sy - 2.0, W - 2.0 * MARGIN - 90.0 - 70.0, 4.0);
        let t = (s.low_threshold as f32 - 5.0) / 25.0;
        let tx = track.left + t * (track.right - track.left);
        self.p.fill(track, 2.0, self.theme.control_border())?;
        self.p.fill(rect(track.left, track.top, tx - track.left, 4.0), 2.0, self.accent)?;
        self.p.circle(tx, sy, 9.0, self.accent)?;
        self.p.circle(tx, sy, 4.0, self.theme.on_accent())?;
        self.p.text(&format!("{}%", s.low_threshold), 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(right - 56.0, sy - 10.0, 40.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        self.hits.push((rect(track.left - 10.0, sy - 14.0, track.right - track.left + 20.0, 28.0), Act::Threshold));
        y += 96.0 + ROW_GAP;
        let cy = self.row(&mut y, "Teli töltés értesítés", None)?;
        self.switch(right, cy, s.notify_full, Act::NotifyFull)?;
        let cy = self.row(&mut y, "Csendes mód teljes képernyőn", Some("Játék vagy film közben az értesítések várnak"))?;
        self.switch(right, cy, s.quiet_fullscreen, Act::Quiet)?;
        y += SECTION_GAP - ROW_GAP;

        // Teljesítmény
        self.section(&mut y, "Teljesítmény")?;
        let cy = self.row(&mut y, "Frissítési gyakoriság", Some("Ritkább frissítés = kevesebb erőforrás"))?;
        let poll = match s.poll_secs {
            ..=30 => "30",
            31..=60 => "60",
            _ => "300",
        };
        self.segmented(right, cy, &[("30", "30 másodperc"), ("60", "1 perc"), ("300", "5 perc")], poll, |k| Act::Poll(k.parse().unwrap_or(60)))?;
        let cy = self.row(&mut y, "Frissítések", Some("Új NapCat verzió a GitHubról"))?;
        self.segmented(right, cy, &[("auto", "Automatikus"), ("notify", "Értesítés"), ("off", "Ki")], &s.update_mode, Act::Updates)?;
        Ok(y + 32.0 - ROW_GAP)
    }
}

unsafe fn paint(w: &mut Window) -> Result<()> {
    let mut rc = RECT::default();
    GetClientRect(w.hwnd, &mut rc)?;
    let size = D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 };
    if w.rt.is_none() {
        let props = D2D1_RENDER_TARGET_PROPERTIES { dpiX: w.dpi, dpiY: w.dpi, ..Default::default() };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd: w.hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_NONE };
        w.rt = Some(w.renderer.d2d().CreateHwndRenderTarget(&props, &hwnd_props)?);
    }
    let rt = w.rt.as_ref().unwrap();
    rt.Resize(&size)?;
    rt.SetDpi(w.dpi, w.dpi);
    let ctx: ID2D1DeviceContext = rt.cast()?;
    let theme = Theme::ui();
    ctx.BeginDraw();
    ctx.Clear(Some(&theme.window()));
    // A görgetést egy eltolással kezeljük; a rajzoló segédek a jelenlegi transzformációra rajzolnak.
    let mut ui = Ui { p: Painter { ctx: &ctx, renderer: &w.renderer }, theme, accent: theme.accent(), hits: Vec::new(), scroll: w.scroll };
    ui.reset();
    let result = ui.build(&w.settings, w.autostart, &w.devices);
    w.hits = std::mem::take(&mut ui.hits);
    if let Ok(h) = result {
        w.content_h = h;
    }
    ctx.SetTransform(&Matrix3x2::identity());
    if let Err(e) = ctx.EndDraw(None, None) {
        if e.code() == D2DERR_RECREATE_TARGET {
            w.rt = None;
        }
    }
    Ok(())
}


fn client_h(w: &Window) -> f32 {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(w.hwnd, &mut rc);
    }
    (rc.bottom - rc.top) as f32 * 96.0 / w.dpi
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
                        let max = (w.content_h - client_h(w)).max(0.0);
                        w.scroll = (w.scroll - delta * 64.0).clamp(0.0, max);
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                WINDOW.with_borrow_mut(|w| {
                    let Some(w) = w else { return };
                    let (x, y) = to_content(w, lparam);
                    if let Some((r, Act::Threshold)) = w.hits.iter().find(|(r, _)| contains(r, x, y)).cloned() {
                        let track = rect(r.left + 10.0, r.top, r.right - r.left - 20.0, r.bottom - r.top);
                        w.dragging = Some(track);
                        SetCapture(hwnd);
                        drag_threshold(w, x, track);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                WINDOW.with_borrow_mut(|w| {
                    let Some(w) = w else { return };
                    let (x, y) = to_content(w, lparam);
                    if let Some(track) = w.dragging {
                        drag_threshold(w, x, track);
                    }
                    let over = w.dragging.is_some() || w.hits.iter().any(|(r, _)| contains(r, x, y));
                    SetCursor(LoadCursorW(None, if over { IDC_HAND } else { IDC_ARROW }).unwrap_or_default());
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                WINDOW.with_borrow_mut(|w| {
                    let Some(w) = w else { return };
                    if w.dragging.take().is_some() {
                        let _ = ReleaseCapture();
                        return;
                    }
                    let (x, y) = to_content(w, lparam);
                    if let Some((_, act)) = w.hits.iter().find(|(r, _)| contains(r, x, y)).cloned() {
                        on_act(w, act);
                    }
                });
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
                WINDOW.set(None);
                let _ = K32EmptyWorkingSet(GetCurrentProcess());
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
