//! Bal klikkes felugró panel a design "Tálca + felugró panel" artboardja szerint: fejléc (logó,
//! összesítés, nézetváltó és beállítások gomb), lemerülés figyelmeztetés, eszközlista vagy rács,
//! lábléc (utolsó frissítés, eszköz párosítása).
//!
//! Saját Win32 popup ablak Direct2D-vel. Csak megnyitva létezik: bezáráskor (fókuszvesztés,
//! Esc) az ablak és a rajzoló erőforrásai is felszabadulnak.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;

use crate::i18n::t;

use windows::core::{w, Interface, Result};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::draw::{contains, icon, rect, Painter};
use super::icon_render::{IconRenderer, Layout};
use super::theme::Theme;
use crate::model::{ChargeState, DeviceState};

/// Üzenet a tálca ablaknak: wParam 1 = nézet váltás (lParam: 1 rács, 0 lista), 2 = beállítások.
pub const WM_FLYOUT: u32 = WM_APP + 5;
pub const FLYOUT_GRID: usize = 1;
pub const FLYOUT_SETTINGS: usize = 2;
pub const FLYOUT_DEVICES: usize = 3;

// Méretek DIP-ben (96 DPI-n pixel), a design szerint.
const W: f32 = 380.0;
const PAD: f32 = 16.0;
const GAP: f32 = 14.0;
const HEAD: f32 = 32.0;
const ALERT: f32 = 38.0;
const ROW: f32 = 64.0;
const ROW_GAP: f32 = 2.0;
const RING: f32 = 48.0;
const TILE: f32 = 144.0;
const TILE_GAP: f32 = 8.0;
const TILE_RING: f32 = 64.0;
const FOOT: f32 = 45.0;

/// A kurzor elhagyta az ablakot (TrackMouseEvent); a windows crate a Controls modulban tartja.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Kattintható elemek.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    View,
    Settings,
    Pair,
}

struct Flyout {
    hwnd: HWND,
    owner: HWND,
    devices: Vec<DeviceState>,
    grid: bool,
    /// Hány másodperce jött az utolsó eszközfrissítés.
    updated: Option<u64>,
    renderer: IconRenderer,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: f32,
    anchor_bottom: i32,
    hits: Vec<(Hit, D2D_RECT_F)>,
    hover: Option<Hit>,
}

thread_local! {
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
    /// Mikor zárult be utoljára: a tálcaikonra kattintás előbb bezárja (fókuszvesztés), majd
    /// ugyanaz a kattintás újra megnyitná; így viszont a második kattintás valóban bezárja.
    static CLOSED_AT: Cell<u32> = const { Cell::new(0) };
}

pub fn is_open() -> bool {
    FLYOUT.with_borrow(|f| f.is_some())
}

/// Megnyitja a panelt a tálcaikon (`anchor`) fölött, vagy bezárja, ha nyitva van.
pub unsafe fn toggle(owner: HWND, anchor: RECT, devices: Vec<DeviceState>, grid: bool, updated: Option<u64>) {
    if let Some(hwnd) = FLYOUT.with_borrow(|f| f.as_ref().map(|f| f.hwnd)) {
        let _ = DestroyWindow(hwnd);
        return;
    }
    if GetTickCount().wrapping_sub(CLOSED_AT.get()) < 300 {
        return;
    }
    let _ = open(owner, anchor, devices, grid, updated);
}

/// Friss eszközállapotok egy nyitott panelnek.
pub unsafe fn update(devices: &[DeviceState], updated: Option<u64>) {
    let hwnd = FLYOUT.with_borrow_mut(|f| {
        let f = f.as_mut()?;
        f.devices = devices.to_vec();
        f.updated = updated;
        Some(f.hwnd)
    });
    if let Some(hwnd) = hwnd {
        resize(hwnd);
    }
}

/// A tartalom magassága DIP-ben.
fn content_height(devices: &[DeviceState], grid: bool) -> f32 {
    let n = devices.len();
    let list = if n == 0 {
        40.0
    } else if grid {
        let rows = n.div_ceil(3) as f32;
        rows * TILE + (rows - 1.0) * TILE_GAP
    } else {
        n as f32 * ROW + (n as f32 - 1.0) * ROW_GAP
    };
    let alert = if alert_device(devices).is_some() { ALERT + GAP } else { 0.0 };
    PAD + HEAD + GAP + alert + list + GAP + FOOT + PAD
}

fn size_px(dpi: f32, devices: &[DeviceState], grid: bool) -> (i32, i32) {
    let scale = dpi / 96.0;
    ((W * scale).round() as i32, (content_height(devices, grid) * scale).round() as i32)
}

/// Az első lemerülőben lévő (20% alatti, nem töltő) eszköz.
fn alert_device(devices: &[DeviceState]) -> Option<&DeviceState> {
    devices.iter().find(|d| d.connected && d.charge == ChargeState::Discharging && d.level.is_some_and(|l| l < 20))
}

/// Magyar névelő: "az" magánhangzóval kezdődő név előtt.
fn article(name: &str) -> &'static str {
    let first = name.chars().next().map(|c| c.to_lowercase().next().unwrap_or(c));
    if first.is_some_and(|c| "aáeéiíoóöőuúüű".contains(c)) { "Az" } else { "A" }
}

/// Az állapotsor a név alatt, a design szerint a becsléssel: "Lemerülőben · kb. 40 perc",
/// "Töltés · 1 ó 10 p a telig", "Nincs csatlakoztatva · 2 órája".
fn status_text(d: &DeviceState) -> String {
    match &d.estimate {
        Some(e) => format!("{} · {e}", status_base(d)),
        None => status_base(d).to_string(),
    }
}

fn status_base(d: &DeviceState) -> &'static str {
    match (d.connected, d.charge, d.level) {
        (false, _, _) => t("Nincs csatlakoztatva", "Not connected"),
        (true, ChargeState::Charging, _) => t("Töltés", "Charging"),
        (true, ChargeState::Full, _) => t("Feltöltve", "Fully charged"),
        (true, ChargeState::Discharging, Some(l)) if l < 20 => t("Lemerülőben", "Low battery"),
        (true, ChargeState::Discharging, _) => t("Nem tölt", "Not charging"),
        (true, ChargeState::Unknown, None) => t("Csatlakoztatva, szint ismeretlen", "Connected, level unknown"),
        (true, ChargeState::Unknown, Some(_)) => t("Csatlakoztatva", "Connected"),
    }
}

fn updated_text(updated: Option<u64>) -> String {
    let label = t("Frissítve", "Updated");
    match updated {
        None => format!("{label}: –"),
        Some(s) if s < 60 => format!("{label}: {}", t("most", "now")),
        Some(s) if s < 3600 => format!("{label}: {} {}", s / 60, t("perce", "min ago")),
        Some(s) => format!("{label}: {} {}", s / 3600, t("órája", "h ago")),
    }
}

unsafe fn open(owner: HWND, anchor: RECT, devices: Vec<DeviceState>, grid: bool, updated: Option<u64>) -> Result<()> {
    let hinst = GetModuleHandleW(None)?;
    let class = w!("NapCatFlyout");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: class,
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        ..Default::default()
    };
    RegisterClassW(&wc); // második hívásra "már létezik" hibát ad, az rendben van

    // A tálcaikon monitorának DPI-je szerint méretezünk.
    let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let dpi = dx as f32;
    let (w, h) = size_px(dpi, &devices, grid);

    // A tálca fölé (vagy mellé, ha függőleges), a munkaterületen belül.
    let point = POINT { x: (anchor.left + anchor.right) / 2, y: (anchor.top + anchor.bottom) / 2 };
    let mut pos = RECT::default();
    let flags = TPM_CENTERALIGN | TPM_BOTTOMALIGN | TPM_VERTICAL | TPM_WORKAREA;
    if CalculatePopupWindowPosition(&point, &SIZE { cx: w, cy: h }, flags.0, Some(&anchor), &mut pos).is_err() {
        pos = RECT { left: point.x - w / 2, top: anchor.top - h, right: point.x + w / 2, bottom: anchor.top };
    }

    let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, class, w!("NapCat"), WS_POPUP, pos.left, pos.top, w, h, None, None, hinst, None)?;
    let theme = Theme::ui();
    set_dwm_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0); // Win11; Win10-en hatástalan
    set_dwm_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (!theme.light) as i32);
    // A design 1 px-es halvány kerete (a panel színére keverve), COLORREF: 0x00BBGGRR.
    set_dwm_attr(hwnd, DWMWA_BORDER_COLOR, if theme.light { 0x00E4E4E5 } else { 0x00373632 });

    FLYOUT.set(Some(Flyout {
        hwnd,
        owner,
        devices,
        grid,
        updated,
        renderer: IconRenderer::new()?,
        rt: None,
        dpi,
        anchor_bottom: pos.top + h,
        hits: Vec::new(),
        hover: None,
    }));
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    Ok(())
}

/// Méret a tartalomhoz, az alsó szél a helyén marad (a panel a tálca fölött nő vagy zsugorodik).
unsafe fn resize(hwnd: HWND) {
    let size = FLYOUT.with_borrow(|f| f.as_ref().map(|f| (size_px(f.dpi, &f.devices, f.grid), f.anchor_bottom)));
    if let Some(((w, h), bottom)) = size {
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        let _ = SetWindowPos(hwnd, None, rc.left, bottom - h, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe fn set_dwm_attr(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: i32) -> bool {
    DwmSetWindowAttribute(hwnd, attr, &value as *const i32 as *const c_void, size_of::<i32>() as u32).is_ok()
}

unsafe fn paint(f: &mut Flyout) -> Result<()> {
    let mut rc = RECT::default();
    GetClientRect(f.hwnd, &mut rc)?;
    let size = D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 };
    if f.rt.is_none() {
        // Szoftveres megjelenítés: a kis felülethez nem kell GPU, és így nem töltődik be a videókártya-
        // meghajtó (Direct3D), ami bezárás után is ~20 MB-ot foglalna.
        let props = D2D1_RENDER_TARGET_PROPERTIES { r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, dpiX: f.dpi, dpiY: f.dpi, ..Default::default() };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd: f.hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_NONE };
        f.rt = Some(f.renderer.d2d().CreateHwndRenderTarget(&props, &hwnd_props)?);
    }
    let rt = f.rt.as_ref().unwrap();
    rt.Resize(&size)?;
    let ctx: ID2D1DeviceContext = rt.cast()?;
    ctx.BeginDraw();
    f.hits = draw_content(&ctx, &f.renderer, &f.devices, f.grid, f.updated, f.hover, Theme::ui())?;
    if let Err(e) = ctx.EndDraw(None, None) {
        if e.code() == D2DERR_RECREATE_TARGET {
            f.rt = None;
        }
    }
    Ok(())
}

/// A panel tartalma DIP-ben; az ablak és az előnézeti teszt is ezt rajzolja. A kattintható
/// elemek helyét adja vissza.
unsafe fn draw_content(
    ctx: &ID2D1DeviceContext,
    renderer: &IconRenderer,
    devices: &[DeviceState],
    grid: bool,
    updated: Option<u64>,
    hover: Option<Hit>,
    theme: Theme,
) -> Result<Vec<(Hit, D2D_RECT_F)>> {
    let p = Painter { ctx, renderer };
    let (fg, sub, accent) = (theme.fg(1.0), theme.sub(), theme.accent());
    ctx.Clear(Some(&theme.panel()));
    let mut hits = Vec::new();
    let button = |hit: Hit, r: D2D_RECT_F, hits: &mut Vec<(Hit, D2D_RECT_F)>| -> Result<()> {
        if hover == Some(hit) {
            p.fill(r, 6.0, theme.tile())?;
        }
        hits.push((hit, r));
        Ok(())
    };

    // Fejléc: logó, cím, összesítés, nézetváltó és beállítások gomb.
    let mut y = PAD;
    p.logo(PAD, y + 2.0, 28.0)?;
    p.text("NapCat", 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(PAD + 36.0, y, 80.0, HEAD), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
    let charging = devices.iter().filter(|d| d.is_charging()).count();
    let summary = match (devices.len(), charging) {
        (0, _) => t("nincs eszköz", "no devices").to_string(),
        (n, 0) => format!("{n} {}", t("eszköz", if n == 1 { "device" } else { "devices" })),
        (n, c) => format!("{n} {} · {c} {}", t("eszköz", if n == 1 { "device" } else { "devices" }), t("tölt", "charging")),
    };
    let title_w = p.text_width("NapCat", 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
    p.text(&summary, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(PAD + 36.0 + title_w + 8.0, y, 160.0, HEAD), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
    let settings_r = rect(W - PAD - 32.0, y, 32.0, 32.0);
    let view_r = rect(W - PAD - 66.0, y, 32.0, 32.0);
    button(Hit::View, view_r, &mut hits)?;
    button(Hit::Settings, settings_r, &mut hits)?;
    p.icon(if grid { icon::LIST } else { icon::GRID }, view_r.left + 8.0, y + 8.0, 16.0, 1.8, sub)?;
    p.icon(icon::GEAR, settings_r.left + 8.0, y + 8.0, 16.0, 1.8, sub)?;
    y += HEAD + GAP;

    // Lemerülés figyelmeztetés.
    if let Some(d) = alert_device(devices) {
        p.fill(rect(PAD, y, W - 2.0 * PAD, ALERT), 8.0, theme.alert_bg())?;
        p.icon(icon::WARNING, PAD + 12.0, y + 11.0, 16.0, 2.0, theme.red())?;
        let left = match &d.estimate {
            Some(e) => e.clone(),
            None => d.level_text().unwrap_or_default(),
        };
        let msg = if crate::i18n::english() {
            format!("{} is running low – {left} left", d.name)
        } else {
            format!("{} {} lemerülőben – {left} van hátra", article(&d.name), d.name)
        };
        p.text(&msg, 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(PAD + 38.0, y, W - 2.0 * PAD - 50.0, ALERT), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        y += ALERT + GAP;
    }

    // Eszközök: lista vagy rács.
    let pct = |d: &DeviceState| match (d.connected, d.level) {
        (true, Some(l)) if l < 20 && d.charge != ChargeState::Charging => (d.level_text().unwrap_or_default(), theme.red()),
        (true, Some(_)) => (d.level_text().unwrap_or_default(), fg),
        _ => ("–".to_string(), sub),
    };
    if devices.is_empty() {
        p.text(t("Csatlakoztass egy támogatott eszközt.", "Connect a supported device."), 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(PAD + 8.0, y, W - 2.0 * PAD, 40.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        y += 40.0;
    } else if grid {
        let tw = (W - 2.0 * PAD - 2.0 * TILE_GAP) / 3.0;
        for (i, d) in devices.iter().enumerate() {
            let (col, row) = ((i % 3) as f32, (i / 3) as f32);
            let (x, ty) = (PAD + col * (tw + TILE_GAP), y + row * (TILE + TILE_GAP));
            p.fill(rect(x, ty, tw, TILE), 10.0, theme.tile())?;
            ctx.SetTransform(&Matrix3x2::translation(x + (tw - TILE_RING) / 2.0, ty + 14.0));
            renderer.draw(ctx, d, TILE_RING, theme, Layout::Ring, Some(theme.tile()))?;
            ctx.SetTransform(&Matrix3x2::identity());
            let (text, color) = pct(d);
            p.text(&text, 16.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x + 4.0, ty + 86.0, tw - 8.0, 22.0), color, DWRITE_TEXT_ALIGNMENT_CENTER)?;
            p.text(&d.name, 11.5, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 6.0, ty + 114.0, tw - 12.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        }
        let rows = devices.len().div_ceil(3) as f32;
        y += rows * TILE + (rows - 1.0) * TILE_GAP;
    } else {
        for d in devices {
            ctx.SetTransform(&Matrix3x2::translation(PAD + 8.0, y + 8.0));
            renderer.draw(ctx, d, RING, theme, Layout::Ring, Some(theme.panel()))?;
            ctx.SetTransform(&Matrix3x2::identity());
            let x = PAD + 8.0 + RING + 14.0;
            let text_w = W - PAD - 8.0 - 70.0 - x;
            let name_color = if d.connected { fg } else { sub };
            p.text(&d.name, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x, y + 11.0, text_w, 20.0), name_color, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            p.circle(x + 3.0, y + 41.0, 3.0, theme.ring(d))?;
            p.text(&status_text(d), 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 12.0, y + 33.0, text_w - 12.0, 17.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            let (text, color) = pct(d);
            p.text(&text, 17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(W - PAD - 8.0 - 70.0, y, 70.0, ROW), color, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            y += ROW + ROW_GAP;
        }
        y -= ROW_GAP;
    }
    y += GAP;

    // Lábléc: elválasztó, utolsó frissítés, eszköz párosítása gomb.
    p.line(PAD, y + 0.5, W - PAD, y + 0.5, theme.border())?;
    let fy = y + 13.0;
    p.text(&updated_text(updated), 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(PAD, fy, 160.0, 32.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
    let label = t("Eszköz párosítása", "Pair a device");
    let bw = 12.0 + 14.0 + 6.0 + p.text_width(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM)? + 12.0;
    let pair_r = rect(W - PAD - bw, fy, bw, 32.0);
    let accent_bg = if hover == Some(Hit::Pair) { D2D1_COLOR_F { a: 0.85, ..accent } } else { accent };
    p.fill(pair_r, 6.0, accent_bg)?;
    p.icon(icon::PLUS, pair_r.left + 12.0, fy + 9.0, 14.0, 2.0, theme.on_accent())?;
    p.text(label, 12.5, DWRITE_FONT_WEIGHT_MEDIUM, rect(pair_r.left + 32.0, fy, bw - 38.0, 32.0), theme.on_accent(), DWRITE_TEXT_ALIGNMENT_LEADING)?;
    hits.push((Hit::Pair, pair_r));
    Ok(hits)
}

/// A panel mérete DIP-ben és rajzolása a képernyőképekhez (ugyanaz a kód, mint az ablaké).
#[cfg(test)]
pub fn gallery_size(devices: &[DeviceState], grid: bool) -> (f32, f32) {
    (W, content_height(devices, grid))
}

#[cfg(test)]
pub unsafe fn gallery_draw(ctx: &ID2D1DeviceContext, renderer: &IconRenderer, devices: &[DeviceState], grid: bool, updated: Option<u64>, theme: Theme) -> Result<()> {
    draw_content(ctx, renderer, devices, grid, updated, None, theme).map(|_| ())
}

/// A kurzor alatti kattintható elem (lParam: kliens koordináták pixelben).
fn hit_at(lparam: LPARAM) -> Option<Hit> {
    let (x, y) = ((lparam.0 & 0xFFFF) as i16 as f32, ((lparam.0 >> 16) & 0xFFFF) as i16 as f32);
    FLYOUT.with_borrow(|f| {
        let f = f.as_ref()?;
        let scale = f.dpi / 96.0;
        f.hits.iter().find(|(_, r)| contains(r, x / scale, y / scale)).map(|(h, _)| *h)
    })
}

unsafe fn on_click(hwnd: HWND, hit: Hit) {
    match hit {
        Hit::View => {
            let state = FLYOUT.with_borrow_mut(|f| {
                let f = f.as_mut()?;
                f.grid = !f.grid;
                Some((f.owner, f.grid))
            });
            if let Some((owner, grid)) = state {
                let _ = PostMessageW(owner, WM_FLYOUT, WPARAM(FLYOUT_GRID), LPARAM(grid as isize));
                resize(hwnd);
            }
        }
        Hit::Settings => {
            if let Some(owner) = FLYOUT.with_borrow(|f| f.as_ref().map(|f| f.owner)) {
                let _ = PostMessageW(owner, WM_FLYOUT, WPARAM(FLYOUT_SETTINGS), LPARAM(0));
            }
            let _ = DestroyWindow(hwnd);
        }
        Hit::Pair => {
            // A NapCat Eszközök oldala: észlelt eszközök hozzáadása, eltávolítás, Bluetooth párosítás.
            if let Some(owner) = FLYOUT.with_borrow(|f| f.as_ref().map(|f| f.owner)) {
                let _ = PostMessageW(owner, WM_FLYOUT, WPARAM(FLYOUT_DEVICES), LPARAM(0));
            }
            let _ = DestroyWindow(hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DeviceKind;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};

    /// Panel előnézet (lista és rács, sötét és világos), 2x méretben: target/flyout-preview-*.png.
    #[test]
    fn flyout_preview() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let renderer = IconRenderer::new().unwrap();
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let dev = |name: &str, kind, level, charge, connected| DeviceState {
                id: name.into(),
                name: name.into(),
                kind,
                level,
                level_approx: false,
                charge,
                connected,
                hw: None, estimate: None,
            };
            let devices = vec![
                dev("DualShock 4", DeviceKind::Controller, Some(12), ChargeState::Discharging, true),
                dev("Logitech G435", DeviceKind::Headset, Some(46), ChargeState::Charging, true),
                dev("VGN Dragonfly", DeviceKind::Mouse, Some(78), ChargeState::Discharging, true),
                dev("Royal Kludge R75 Pro", DeviceKind::Keyboard, Some(100), ChargeState::Full, true),
                dev("Xbox kontroller", DeviceKind::Controller, None, ChargeState::Unknown, false),
            ];
            for (name, light, grid) in [("dark", false, false), ("light", true, false), ("grid", false, true)] {
                let (w, h) = size_px(192.0, &devices, grid);
                let bmp = wic.CreateBitmap(w as u32, h as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand).unwrap();
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 192.0,
                    dpiY: 192.0,
                    ..Default::default()
                };
                let ctx: ID2D1DeviceContext = renderer.d2d().CreateWicBitmapRenderTarget(&bmp, &props).unwrap().cast().unwrap();
                ctx.BeginDraw();
                draw_content(&ctx, &renderer, &devices, grid, Some(5), None, Theme { light }).unwrap();
                ctx.EndDraw(None, None).unwrap();

                let path: Vec<u16> =
                    format!("{}\\target\\flyout-preview-{name}.png\0", env!("CARGO_MANIFEST_DIR")).encode_utf16().collect();
                let stream = wic.CreateStream().unwrap();
                stream.InitializeFromFilename(windows::core::PCWSTR(path.as_ptr()), GENERIC_WRITE.0).unwrap();
                let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
                enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
                let (mut frame, mut bag) = (None, None);
                enc.CreateNewFrame(&mut frame, &mut bag).unwrap();
                let frame = frame.unwrap();
                frame.Initialize(bag.as_ref()).unwrap();
                frame.SetSize(w as u32, h as u32).unwrap();
                let mut fmt = GUID_WICPixelFormat32bppPBGRA;
                frame.SetPixelFormat(&mut fmt).unwrap();
                frame.WriteSource(&bmp, std::ptr::null()).unwrap();
                frame.Commit().unwrap();
                enc.Commit().unwrap();
            }
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                FLYOUT.with_borrow_mut(|f| {
                    if let Some(f) = f {
                        let _ = paint(f);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                let hover = hit_at(lparam);
                let changed = FLYOUT.with_borrow_mut(|f| f.as_mut().is_some_and(|f| std::mem::replace(&mut f.hover, hover) != hover));
                if changed {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
                SetCursor(LoadCursorW(None, if hover.is_some() { IDC_HAND } else { IDC_ARROW }).unwrap_or_default());
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                FLYOUT.with_borrow_mut(|f| {
                    if let Some(f) = f {
                        f.hover = None;
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(hit) = hit_at(lparam) {
                    on_click(hwnd, hit);
                }
                LRESULT(0)
            }
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                FLYOUT.set(None);
                CLOSED_AT.set(GetTickCount());
                let _ = K32EmptyWorkingSet(GetCurrentProcess());
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
