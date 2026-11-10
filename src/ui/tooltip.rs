//! Gazdag tooltip a design "3 · Rámutatás" része szerint: a tálcaikonra mutatva, kattintás nélkül
//! minden eszköz egy pillantásra (kis gyűrű, név, százalék). A Windows saját egysoros tooltipje
//! helyett jelenik meg (NIN_POPUPOPEN / NIN_POPUPCLOSE), nem kap fókuszt, és átengedi az egeret.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::draw::{rect, Painter};
use super::icon_render::IconRenderer;
use super::theme::Theme;
use crate::model::{ChargeState, DeviceState};

const W: f32 = 232.0;
const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 10.0;
const ROW: f32 = 20.0;
const GAP: f32 = 7.0;

struct Tip {
    hwnd: HWND,
    devices: Vec<DeviceState>,
    renderer: IconRenderer,
    dpi: f32,
}

thread_local! {
    static TIP: RefCell<Option<Tip>> = const { RefCell::new(None) };
}

fn height(n: usize) -> f32 {
    2.0 * PAD_Y + n.max(1) as f32 * ROW + n.saturating_sub(1) as f32 * GAP
}

/// Megjelenítés a tálcaikon (`anchor`) fölött; ha már látszik, csak frissül és áthelyeződik.
pub unsafe fn show(anchor: RECT, devices: Vec<DeviceState>) {
    hide();
    let _ = create(anchor, devices);
}

pub unsafe fn hide() {
    if let Some(hwnd) = TIP.with_borrow(|t| t.as_ref().map(|t| t.hwnd)) {
        let _ = DestroyWindow(hwnd);
    }
}

unsafe fn create(anchor: RECT, devices: Vec<DeviceState>) -> Result<()> {
    let hinst = GetModuleHandleW(None)?;
    let class = w!("NapCatTip");
    let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassW(&wc);

    let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let dpi = dx as f32;
    let scale = dpi / 96.0;
    let (w, h) = ((W * scale).round() as i32, (height(devices.len()) * scale).round() as i32);
    let point = POINT { x: (anchor.left + anchor.right) / 2, y: (anchor.top + anchor.bottom) / 2 };
    let mut pos = RECT::default();
    let flags = TPM_CENTERALIGN | TPM_BOTTOMALIGN | TPM_VERTICAL | TPM_WORKAREA;
    let mut exclude = anchor;
    exclude.top -= (8.0 * scale) as i32; // kis rés az ikon és a tooltip között
    if CalculatePopupWindowPosition(&point, &SIZE { cx: w, cy: h }, flags.0, Some(&exclude), &mut pos).is_err() {
        pos = RECT { left: point.x - w / 2, top: exclude.top - h, right: point.x + w / 2, bottom: exclude.top };
    }
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        class,
        w!("NapCat"),
        WS_POPUP,
        pos.left,
        pos.top,
        w,
        h,
        None,
        None,
        hinst,
        None,
    )?;
    let theme = Theme::ui();
    let set = |attr: DWMWINDOWATTRIBUTE, v: i32| {
        let _ = DwmSetWindowAttribute(hwnd, attr, &v as *const i32 as *const c_void, size_of::<i32>() as u32);
    };
    set(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUNDSMALL.0);
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, (!theme.light) as i32);
    set(DWMWA_BORDER_COLOR, if theme.light { 0x00E4E4E5 } else { 0x00403F3B });
    TIP.set(Some(Tip { hwnd, devices, renderer: IconRenderer::new()?, dpi }));
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    Ok(())
}

unsafe fn paint(t: &Tip) -> Result<()> {
    let mut rc = RECT::default();
    GetClientRect(t.hwnd, &mut rc)?;
    let size = D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 };
    let props = D2D1_RENDER_TARGET_PROPERTIES { r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, dpiX: t.dpi, dpiY: t.dpi, ..Default::default() };
    let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd: t.hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_NONE };
    let rt = t.renderer.d2d().CreateHwndRenderTarget(&props, &hwnd_props)?;
    let ctx: ID2D1DeviceContext = rt.cast()?;
    ctx.BeginDraw();
    draw(&ctx, &t.renderer, &t.devices, Theme::ui())?;
    ctx.EndDraw(None, None)?;
    Ok(())
}

/// A tooltip mérete DIP-ben (a képernyőképekhez).
#[cfg(test)]
pub fn size(n: usize) -> (f32, f32) {
    (W, height(n))
}

/// A tooltip tartalma DIP-ben; az ablak és a képernyőképek is ezt rajzolják.
pub unsafe fn draw(ctx: &ID2D1DeviceContext, renderer: &IconRenderer, devices: &[DeviceState], theme: Theme) -> Result<()> {
    let p = Painter { ctx, renderer };
    // A design tooltip háttere: a panelnél egy árnyalattal világosabb.
    ctx.Clear(Some(&if theme.light { super::theme::rgb(0xFFFFFF, 1.0) } else { super::theme::rgb(0x26272C, 1.0) }));
    let mut y = PAD_Y;
    if devices.is_empty() {
        p.text(crate::i18n::t("Nincs eszköz", "No devices"), 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(PAD_X, y, W - 2.0 * PAD_X, ROW), theme.sub(), DWRITE_TEXT_ALIGNMENT_LEADING)?;
    }
    for d in devices {
        // Kis gyűrű: halvány háttérkör, rajta a szint íve (leválasztva szaggatott, ív nélkül).
        let (c, r, sw) = (ROW / 2.0, ROW * 20.0 / 48.0, ROW * 8.0 / 48.0);
        let track = D2D1_ELLIPSE { point: D2D_POINT_2F { x: PAD_X + c, y: y + c }, radiusX: r, radiusY: r };
        let style = if d.connected { None } else { Some(renderer.stroke_style(&[0.5, 0.6])?) };
        ctx.DrawEllipse(&track, &p.brush(&theme.track())?, sw, style.as_ref());
        if let (true, Some(level)) = (d.connected, d.level) {
            ctx.SetTransform(&windows::Foundation::Numerics::Matrix3x2::translation(PAD_X, y));
            renderer.arc(ctx, c, r, sw, level as f32 / 100.0, &p.brush(&theme.ring(d))?)?;
            ctx.SetTransform(&windows::Foundation::Numerics::Matrix3x2::identity());
        }
        let name_color = if d.connected { theme.fg(1.0) } else { theme.sub() };
        p.text(&d.name, 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(PAD_X + ROW + 9.0, y, W - 2.0 * PAD_X - ROW - 9.0 - 48.0, ROW), name_color, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let (pct, color) = match (d.connected, d.level) {
            (true, Some(l)) if l < 20 && d.charge != ChargeState::Charging => (d.level_text().unwrap_or_default(), theme.red()),
            (true, Some(_)) => (d.level_text().unwrap_or_default(), theme.fg(1.0)),
            _ => ("–".to_string(), theme.sub()),
        };
        p.text(&pct, 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(W - PAD_X - 48.0, y, 48.0, ROW), color, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        y += ROW + GAP;
    }
    Ok(())
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                TIP.with_borrow(|t| {
                    if let Some(t) = t {
                        let _ = paint(t);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_NCHITTEST => LRESULT(-1), // HTTRANSPARENT: az egér átmegy rajta
            WM_DESTROY => {
                TIP.set(None);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
