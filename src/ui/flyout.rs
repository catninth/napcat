//! Bal klikkes felugró panel: az eszközök listája nagyobb gyűrűkkel, névvel, állapottal.
//!
//! Saját Win32 popup ablak Direct2D-vel. Csak megnyitva létezik: bezáráskor (fókuszvesztés,
//! Esc) az ablak és a rajzoló erőforrásai is felszabadulnak.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;

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
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::icon_render::IconRenderer;
use super::theme::Theme;
use crate::model::{ChargeState, DeviceState};

// Méretek DIP-ben (96 DPI-n pixel).
const WIDTH: f32 = 320.0;
const PAD: f32 = 12.0;
const HEADER: f32 = 52.0;
const ROW: f32 = 60.0;
const RING: f32 = 40.0;

struct Flyout {
    hwnd: HWND,
    devices: Vec<DeviceState>,
    renderer: IconRenderer,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: f32,
    anchor_bottom: i32,
}

thread_local! {
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
    /// Mikor zárult be utoljára: a tálcaikonra kattintás előbb bezárja (fókuszvesztés), majd
    /// ugyanaz a kattintás újra megnyitná; így viszont a második kattintás valóban bezárja.
    static CLOSED_AT: Cell<u32> = const { Cell::new(0) };
}

/// Megnyitja a panelt a tálcaikon (`anchor`) fölött, vagy bezárja, ha nyitva van.
pub unsafe fn toggle(anchor: RECT, devices: Vec<DeviceState>) {
    if let Some(hwnd) = FLYOUT.with_borrow(|f| f.as_ref().map(|f| f.hwnd)) {
        let _ = DestroyWindow(hwnd);
        return;
    }
    if GetTickCount().wrapping_sub(CLOSED_AT.get()) < 300 {
        return;
    }
    let _ = open(anchor, devices);
}

/// Friss eszközállapotok egy nyitott panelnek.
pub unsafe fn update(devices: &[DeviceState]) {
    let resize = FLYOUT.with_borrow_mut(|f| {
        let f = f.as_mut()?;
        let resize = f.devices.len() != devices.len();
        f.devices = devices.to_vec();
        let _ = InvalidateRect(f.hwnd, None, false);
        resize.then_some((f.hwnd, f.dpi, f.anchor_bottom, devices.len()))
    });
    if let Some((hwnd, dpi, bottom, n)) = resize {
        let (w, h) = size_px(dpi, n);
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        let _ = SetWindowPos(hwnd, None, rc.right - w, bottom - h, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn size_px(dpi: f32, rows: usize) -> (i32, i32) {
    let scale = dpi / 96.0;
    let h = HEADER + ROW * rows.max(1) as f32 + PAD;
    ((WIDTH * scale).round() as i32, (h * scale).round() as i32)
}

unsafe fn open(anchor: RECT, devices: Vec<DeviceState>) -> Result<()> {
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
    let (w, h) = size_px(dpi, devices.len());

    // A tálca fölé (vagy mellé, ha függőleges), a munkaterületen belül.
    let point = POINT { x: (anchor.left + anchor.right) / 2, y: (anchor.top + anchor.bottom) / 2 };
    let mut pos = RECT::default();
    let flags = TPM_CENTERALIGN | TPM_BOTTOMALIGN | TPM_VERTICAL | TPM_WORKAREA;
    if CalculatePopupWindowPosition(&point, &SIZE { cx: w, cy: h }, flags.0, Some(&anchor), &mut pos).is_err() {
        pos = RECT { left: point.x - w / 2, top: anchor.top - h, right: point.x + w / 2, bottom: anchor.top };
    }

    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
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
    let theme = Theme::current();
    set_dwm_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0); // Win11; Win10-en hatástalan
    set_dwm_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (!theme.light) as i32);

    FLYOUT.set(Some(Flyout { hwnd, devices, renderer: IconRenderer::new()?, rt: None, dpi, anchor_bottom: pos.top + h }));
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
    Ok(())
}

unsafe fn set_dwm_attr(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: i32) {
    let _ = DwmSetWindowAttribute(hwnd, attr, &value as *const i32 as *const c_void, size_of::<i32>() as u32);
}

unsafe fn paint(f: &mut Flyout) -> Result<()> {
    let mut rc = RECT::default();
    GetClientRect(f.hwnd, &mut rc)?;
    let size = D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 };
    if f.rt.is_none() {
        let props = D2D1_RENDER_TARGET_PROPERTIES { dpiX: f.dpi, dpiY: f.dpi, ..Default::default() };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd: f.hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_NONE };
        f.rt = Some(f.renderer.d2d().CreateHwndRenderTarget(&props, &hwnd_props)?);
    }
    let rt = f.rt.as_ref().unwrap();
    rt.Resize(&size)?;
    let ctx: ID2D1DeviceContext = rt.cast()?;
    ctx.BeginDraw();
    draw_content(&ctx, &f.renderer, &f.devices, Theme::current())?;
    if let Err(e) = ctx.EndDraw(None, None) {
        if e.code() == D2DERR_RECREATE_TARGET {
            f.rt = None;
        }
    }
    Ok(())
}

/// A panel tartalma DIP-ben; az ablak és az előnézeti teszt is ezt rajzolja.
unsafe fn draw_content(ctx: &ID2D1DeviceContext, renderer: &IconRenderer, devices: &[DeviceState], theme: Theme) -> Result<()> {
    let text = Text { ctx, dwrite: renderer.dwrite() };
    ctx.Clear(Some(&theme.panel()));

    // Fejléc: cím és összesítés.
    let charging = devices.iter().filter(|d| d.is_charging()).count();
    let summary = match (devices.len(), charging) {
        (0, _) => "nincs ismert eszköz".to_string(),
        (n, 0) => format!("{n} eszköz"),
        (n, c) => format!("{n} eszköz · {c} tölt"),
    };
    text.draw("NapCat", 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, PAD + 4.0, 16.0, 120.0, theme.fg(1.0), false)?;
    text.draw(&summary, 12.0, DWRITE_FONT_WEIGHT_NORMAL, PAD + 72.0, 18.5, 220.0, theme.sub(), false)?;

    // Eszközsorok.
    let right = WIDTH - PAD - 4.0;
    for (i, d) in devices.iter().enumerate() {
        let y = HEADER + i as f32 * ROW;
        let ring_y = y + (ROW - RING) / 2.0;
        ctx.SetTransform(&Matrix3x2::translation(PAD + 4.0, ring_y));
        renderer.draw(ctx, d, RING, theme, Some(theme.panel()))?;
        ctx.SetTransform(&Matrix3x2::identity());

        let x = PAD + 4.0 + RING + 14.0;
        let name_color = theme.fg(if d.connected { 1.0 } else { 0.6 });
        text.draw(&d.name, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, x, y + 12.0, right - x - 52.0, name_color, false)?;
        text.draw(d.status(), 12.0, DWRITE_FONT_WEIGHT_NORMAL, x, y + 32.0, right - x - 52.0, theme.sub(), false)?;
        let (pct, pct_color) = match (d.connected, d.level) {
            (true, Some(l)) if l < 20 && d.charge != ChargeState::Charging => (format!("{l}%"), theme.ring(d)),
            (true, Some(l)) => (format!("{l}%"), theme.fg(1.0)),
            _ => ("–".to_string(), theme.sub()),
        };
        text.draw(&pct, 17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, right - 60.0, y + 19.0, 60.0, pct_color, true)?;
    }
    if devices.is_empty() {
        let msg = "Csatlakoztass egy támogatott eszközt.";
        text.draw(msg, 12.5, DWRITE_FONT_WEIGHT_NORMAL, PAD + 4.0, HEADER + 20.0, WIDTH - 2.0 * PAD, theme.sub(), false)?;
    }
    Ok(())
}

struct Text<'a> {
    ctx: &'a ID2D1DeviceContext,
    dwrite: &'a IDWriteFactory,
}

impl Text<'_> {
    /// Egysoros szöveg (x, y) bal felső sarokkal, `width` szélességben; túl hosszúnál "…".
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw(&self, s: &str, size: f32, weight: DWRITE_FONT_WEIGHT, x: f32, y: f32, width: f32, color: D2D1_COLOR_F, right: bool) -> Result<()> {
        let format = self.dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        if right {
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        }
        let ellipsis = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
        let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        format.SetTrimming(&trimming, &ellipsis)?;
        let wide: Vec<u16> = s.encode_utf16().collect();
        let brush = self.ctx.CreateSolidColorBrush(&color, None)?;
        let rect = D2D_RECT_F { left: x, top: y, right: x + width, bottom: y + size * 1.5 };
        self.ctx.DrawText(&wide, &format, &rect, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DeviceKind;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};

    /// Panel előnézet sötét és világos témában, 2x méretben: target/flyout-preview-{dark,light}.png.
    #[test]
    fn flyout_preview() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let renderer = IconRenderer::new().unwrap();
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let dev = |name: &str, kind, level, charge, connected| DeviceState { id: name.into(), name: name.into(), kind, level, charge, connected };
            let devices = vec![
                dev("Royal Kludge R75 Pro", DeviceKind::Keyboard, None, ChargeState::Charging, true),
                dev("VGN Dragonfly", DeviceKind::Mouse, None, ChargeState::Unknown, true),
                dev("Logitech G435", DeviceKind::Headset, Some(46), ChargeState::Discharging, true),
                dev("DualSense", DeviceKind::Controller, Some(12), ChargeState::Discharging, true),
                dev("Xbox kontroller", DeviceKind::Controller, None, ChargeState::Unknown, false),
            ];
            for (name, light) in [("dark", false), ("light", true)] {
                let (w, h) = size_px(192.0, devices.len());
                let bmp = wic.CreateBitmap(w as u32, h as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand).unwrap();
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 192.0,
                    dpiY: 192.0,
                    ..Default::default()
                };
                let ctx: ID2D1DeviceContext = renderer.d2d().CreateWicBitmapRenderTarget(&bmp, &props).unwrap().cast().unwrap();
                ctx.BeginDraw();
                draw_content(&ctx, &renderer, &devices, Theme { light }).unwrap();
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
