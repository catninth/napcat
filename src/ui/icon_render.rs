//! Direct2D + DirectWrite: eszköz glyph + töltöttségi gyűrű -> HICON.

use std::f32::consts::TAU;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{w, Interface, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

use super::theme::{rgb, Theme};
use crate::model::{DeviceKind, DeviceState};

/// Az ikon elrendezése.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// A glyph körül töltöttségi gyűrű. Nagy méretben (panel) a legszebb, de 16 px-en a gyűrű
    /// miatt a glyph csak kb. fele akkora, mint a többi tálcaikoné.
    Ring,
    /// Teljes méretű glyph, a jobb alsó sarokban kis torta jelvény a töltöttséggel
    /// (töltés közben villám), ahogy a tálcaikonok állapotjelvényei szokták.
    Pie,
    /// Teljes szélességű glyph, alatta vékony töltöttségi sáv.
    #[allow(dead_code)] // választható elrendezés, a beállításokból lesz elérhető
    Bar,
}

pub struct IconRenderer {
    d2d: ID2D1Factory1,
    wic: IWICImagingFactory,
    dwrite: IDWriteFactory,
    glyph_font: PCWSTR,
    /// A tálcaikonok (`render`) elrendezése.
    pub tray_layout: Layout,
}

/// Kivágás: a tálcán átlátszóvá tett pixelek (bármilyen háttéren olvasható), átlátszatlan
/// felületen (panel) a háttér színe.
struct Cut {
    brush: ID2D1SolidColorBrush,
    blend: D2D1_PRIMITIVE_BLEND,
}

impl Cut {
    unsafe fn new(rt: &ID2D1DeviceContext, bg: Option<D2D1_COLOR_F>) -> Result<Cut> {
        Ok(match bg {
            None => Cut { brush: rt.CreateSolidColorBrush(&rgb(0, 0.0), None)?, blend: D2D1_PRIMITIVE_BLEND_COPY },
            Some(color) => Cut { brush: rt.CreateSolidColorBrush(&color, None)?, blend: D2D1_PRIMITIVE_BLEND_SOURCE_OVER },
        })
    }

    unsafe fn ellipse(&self, rt: &ID2D1DeviceContext, e: &D2D1_ELLIPSE) {
        rt.SetPrimitiveBlend(self.blend);
        rt.FillEllipse(e, &self.brush);
        rt.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
    }

    unsafe fn geometry(&self, rt: &ID2D1DeviceContext, g: &ID2D1PathGeometry) {
        rt.SetPrimitiveBlend(self.blend);
        rt.FillGeometry(g, &self.brush, None);
        rt.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
    }
}

impl IconRenderer {
    pub fn new() -> Result<IconRenderer> {
        unsafe {
            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            // Win11: Segoe Fluent Icons, Win10: Segoe MDL2 Assets. Ugyanazok a kódpontok.
            let glyph_font = if has_font(&dwrite, w!("Segoe Fluent Icons")) {
                w!("Segoe Fluent Icons")
            } else {
                w!("Segoe MDL2 Assets")
            };
            Ok(IconRenderer { d2d, wic, dwrite, glyph_font, tray_layout: Layout::Pie })
        }
    }

    pub fn render(&self, dev: &DeviceState, size: u32, theme: Theme) -> Result<HICON> {
        unsafe { to_hicon(&self.render_pixels(dev, size, theme)?, size) }
    }

    /// Premultiplied BGRA pixelek, soronként size*4 bájt.
    fn render_pixels(&self, dev: &DeviceState, size: u32, theme: Theme) -> Result<Vec<u8>> {
        unsafe {
            let bmp = self.wic.CreateBitmap(size, size, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let rt: ID2D1DeviceContext = self.d2d.CreateWicBitmapRenderTarget(&bmp, &props)?.cast()?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.BeginDraw();
            rt.Clear(Some(&rgb(0, 0.0)));
            self.draw(&rt, dev, size as f32, theme, self.tray_layout, None)?;
            rt.EndDraw(None, None)?;

            let mut px = vec![0u8; (size * size * 4) as usize];
            bmp.CopyPixels(std::ptr::null(), size * 4, &mut px)?;
            Ok(px)
        }
    }

    pub fn d2d(&self) -> &ID2D1Factory1 {
        &self.d2d
    }

    pub fn dwrite(&self) -> &IDWriteFactory {
        &self.dwrite
    }

    /// Az ikon rajzolása `s` méretben a (0,0) ponttól (más helyre a hívó transzformációjával).
    /// `bg`: a háttér színe, ha átlátszatlan felületre rajzolunk (felugró panel); `None` esetén
    /// a jelvények körüli rés átlátszó kivágás (tálcaikon).
    pub unsafe fn draw(
        &self,
        rt: &ID2D1DeviceContext,
        dev: &DeviceState,
        s: f32,
        theme: Theme,
        layout: Layout,
        bg: Option<D2D1_COLOR_F>,
    ) -> Result<()> {
        let cut = Cut::new(rt, bg)?;
        let ring = rt.CreateSolidColorBrush(&theme.ring(dev), None)?;
        let full = D2D_RECT_F { left: 0.0, top: 0.0, right: s, bottom: s };
        match layout {
            Layout::Ring => {
                let c = s / 2.0;
                let sw = ring_width(s);
                let r = c - sw / 2.0;
                // Háttérgyűrű (leválasztva szaggatott), rajta a szint íve.
                let track = rt.CreateSolidColorBrush(&theme.track(), None)?;
                let track_style = if dev.connected { None } else { Some(self.stroke_style(&[1.2, 1.6])?) };
                let ellipse = D2D1_ELLIPSE { point: D2D_POINT_2F { x: c, y: c }, radiusX: r, radiusY: r };
                rt.DrawEllipse(&ellipse, &track, sw, track_style.as_ref());
                if let (true, Some(level)) = (dev.connected, dev.level) {
                    self.arc(rt, c, r, sw, level as f32 / 100.0, &ring)?;
                }
                // A glyph a gyűrű belsejébe: a befoglaló téglalap átlója a belső átmérő; a glyph-ek
                // sarkai lekerekítettek, ezért kicsit nagyobbra vehető.
                self.glyph(rt, dev, theme, c, c, Fit::Circle(1.12 * (r - sw / 2.0 - s * 0.02)))?;
                if dev.is_charging() {
                    self.bolt_badge(rt, s * 39.0 / 48.0, s * 39.0 / 48.0, s * 8.0 / 48.0, s * 2.5 / 48.0, &ring, &cut)?;
                }
            }
            Layout::Pie => {
                self.glyph(rt, dev, theme, s / 2.0, s / 2.0, Fit::Box(full))?;
                // Jelvény a jobb alsó sarokban: töltés közben villám, különben a szint tortája.
                let rb = s * 0.25;
                let (cx, cy) = (s - rb, s - rb);
                let gap = (s / 16.0).max(1.0);
                if dev.is_charging() {
                    self.bolt_badge(rt, cx, cy, rb, gap, &ring, &cut)?;
                } else if let (true, Some(level)) = (dev.connected, dev.level) {
                    let badge = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: rb, radiusY: rb };
                    cut.ellipse(rt, &D2D1_ELLIPSE { radiusX: rb + gap, radiusY: rb + gap, ..badge });
                    let track = rt.CreateSolidColorBrush(&theme.fg(0.35), None)?;
                    rt.FillEllipse(&badge, &track);
                    self.pie(rt, cx, cy, rb, level as f32 / 100.0, &ring)?;
                }
            }
            Layout::Bar => {
                let bar_h = (s * 0.14).max(2.0);
                let gap = (s / 16.0).max(1.0);
                let area = D2D_RECT_F { bottom: s - bar_h - gap, ..full };
                self.glyph(rt, dev, theme, s / 2.0, area.bottom / 2.0, Fit::Box(area))?;
                if dev.connected {
                    let (x0, x1, y0) = (s * 0.08, s * 0.92, s - bar_h);
                    let radius = bar_h / 2.0;
                    let bar = |right: f32| D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F { left: x0, top: y0, right, bottom: s },
                        radiusX: radius,
                        radiusY: radius,
                    };
                    let track = rt.CreateSolidColorBrush(&theme.fg(0.3), None)?;
                    rt.FillRoundedRectangle(&bar(x1), &track);
                    let fraction = match dev.level {
                        Some(l) => l as f32 / 100.0,
                        None if dev.is_charging() => 1.0,
                        None => 0.0,
                    };
                    if fraction > 0.0 {
                        rt.FillRoundedRectangle(&bar(x0 + (x1 - x0) * fraction.max(bar_h / (x1 - x0))), &ring);
                    }
                }
            }
        }
        Ok(())
    }

    /// Az eszköz glyph-je a tálca szövegszínével, (cx, cy) középponttal. A betű valódi (tinta)
    /// kiterjedését mérjük, így a betűtípus saját térköze nem zsugorítja.
    unsafe fn glyph(&self, rt: &ID2D1DeviceContext, dev: &DeviceState, theme: Theme, cx: f32, cy: f32, fit: Fit) -> Result<()> {
        let brush = rt.CreateSolidColorBrush(&theme.fg(if dev.connected { 1.0 } else { 0.45 }), None)?;
        let text = [glyph_char(dev.kind)];
        let ink = self.glyph_ink(&text, 100.0)?;
        let k = match fit {
            Fit::Circle(radius) => 2.0 * radius / (ink.width * ink.width + ink.height * ink.height).sqrt(),
            Fit::Box(r) => ((r.right - r.left) / ink.width).min((r.bottom - r.top) / ink.height),
        };
        let layout = self.glyph_layout(&text, 100.0 * k)?;
        let origin = D2D_POINT_2F { x: cx - (ink.left + ink.width / 2.0) * k, y: cy - (ink.top + ink.height / 2.0) * k };
        rt.DrawTextLayout(origin, &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NO_SNAP);
        Ok(())
    }

    /// Villám jelvény (töltés): színes kör, benne kivágott villám, körülötte `gap` széles rés.
    #[allow(clippy::too_many_arguments)]
    unsafe fn bolt_badge(&self, rt: &ID2D1DeviceContext, cx: f32, cy: f32, rb: f32, gap: f32, color: &ID2D1SolidColorBrush, cut: &Cut) -> Result<()> {
        let badge = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: rb, radiusY: rb };
        cut.ellipse(rt, &D2D1_ELLIPSE { radiusX: rb + gap, radiusY: rb + gap, ..badge });
        rt.FillEllipse(&badge, color);
        // A villám a design 48-as rácsán, a 8 sugarú jelvény középpontjához képest.
        let bolt = [(1.2, -5.2), (-3.4, 1.0), (-0.1, 1.0), (-0.7, 5.2), (3.9, -1.0), (0.6, -1.0)];
        let k = rb / 8.0;
        let geo = self.d2d.CreatePathGeometry()?;
        let sink = geo.Open()?;
        sink.BeginFigure(D2D_POINT_2F { x: cx + bolt[0].0 * k, y: cy + bolt[0].1 * k }, D2D1_FIGURE_BEGIN_FILLED);
        let pts: Vec<_> = bolt[1..].iter().map(|&(x, y)| D2D_POINT_2F { x: cx + x * k, y: cy + y * k }).collect();
        sink.AddLines(&pts);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        cut.geometry(rt, &geo);
        Ok(())
    }

    /// Torta szelet 12 órától óramutató irányban; `fraction` 0..=1.
    unsafe fn pie(&self, rt: &ID2D1DeviceContext, cx: f32, cy: f32, r: f32, fraction: f32, brush: &ID2D1SolidColorBrush) -> Result<()> {
        if fraction >= 1.0 {
            rt.FillEllipse(&D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: r, radiusY: r }, brush);
        } else if fraction > 0.0 {
            let a = TAU * fraction;
            let geo = self.d2d.CreatePathGeometry()?;
            let sink = geo.Open()?;
            sink.BeginFigure(D2D_POINT_2F { x: cx, y: cy }, D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLine(D2D_POINT_2F { x: cx, y: cy - r });
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: D2D_POINT_2F { x: cx + r * a.sin(), y: cy - r * a.cos() },
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if fraction > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            rt.FillGeometry(&geo, brush, None);
        }
        Ok(())
    }

    /// Ív 12 órától óramutató irányban, lekerekített végekkel; `fraction` 0..=1.
    unsafe fn arc(&self, rt: &ID2D1DeviceContext, c: f32, r: f32, sw: f32, fraction: f32, brush: &ID2D1SolidColorBrush) -> Result<()> {
        let round = self.stroke_style(&[])?;
        if fraction >= 1.0 {
            let ellipse = D2D1_ELLIPSE { point: D2D_POINT_2F { x: c, y: c }, radiusX: r, radiusY: r };
            rt.DrawEllipse(&ellipse, brush, sw, &round);
        } else if fraction > 0.0 {
            let a = TAU * fraction;
            let geo = self.d2d.CreatePathGeometry()?;
            let sink = geo.Open()?;
            sink.BeginFigure(D2D_POINT_2F { x: c, y: c - r }, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: D2D_POINT_2F { x: c + r * a.sin(), y: c - r * a.cos() },
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if fraction > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            rt.DrawGeometry(&geo, brush, sw, &round);
        }
        Ok(())
    }

    unsafe fn glyph_layout(&self, text: &[u16], size: f32) -> Result<IDWriteTextLayout> {
        let format = self.dwrite.CreateTextFormat(
            self.glyph_font,
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        self.dwrite.CreateTextLayout(text, &format, size * 2.0, size * 2.0)
    }

    /// A glyph tintájának befoglaló téglalapja a layout origójához képest, `size` betűméretnél.
    unsafe fn glyph_ink(&self, text: &[u16], size: f32) -> Result<Ink> {
        let layout = self.glyph_layout(text, size)?;
        let oh = layout.GetOverhangMetrics()?;
        let (box_w, box_h) = (layout.GetMaxWidth(), layout.GetMaxHeight());
        Ok(Ink { left: -oh.left, top: -oh.top, width: box_w + oh.right + oh.left, height: box_h + oh.bottom + oh.top })
    }

    /// Lekerekített végű vonalstílus; `dashes` nem üres esetén szaggatott (a vonalvastagság egységében).
    unsafe fn stroke_style(&self, dashes: &[f32]) -> Result<ID2D1StrokeStyle> {
        let props = D2D1_STROKE_STYLE_PROPERTIES1 {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_FLAT,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 1.0,
            dashStyle: if dashes.is_empty() { D2D1_DASH_STYLE_SOLID } else { D2D1_DASH_STYLE_CUSTOM },
            dashOffset: 0.0,
            transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
        };
        let style = self.d2d.CreateStrokeStyle(&props, (!dashes.is_empty()).then_some(dashes))?;
        Ok(style.into())
    }
}

struct Ink {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

/// Hová kell beférnie a glyph-nek.
enum Fit {
    /// Ekkora sugarú körbe (a befoglaló téglalap fél átlója).
    Circle(f32),
    /// Ebbe a téglalapba, az arányait megtartva.
    Box(D2D_RECT_F),
}

/// Kis méretben arányaiban vastagabb gyűrű, hogy 16 px-en is olvasható maradjon (16 px -> 2 px).
fn ring_width(s: f32) -> f32 {
    let units = match s as u32 {
        ..=20 => 6.0,
        21..=24 => 5.0,
        _ => 4.0,
    };
    s * units / 48.0
}

/// Segoe Fluent Icons / Segoe MDL2 Assets kódpontok.
fn glyph_char(kind: DeviceKind) -> u16 {
    match kind {
        DeviceKind::Mouse => 0xE962,
        DeviceKind::Keyboard => 0xE765,
        DeviceKind::Headset => 0xE7F6,
        DeviceKind::Controller => 0xE7FC,
        DeviceKind::Other => 0xE772,
    }
}

unsafe fn has_font(dwrite: &IDWriteFactory, family: PCWSTR) -> bool {
    let mut fonts = None;
    if dwrite.GetSystemFontCollection(&mut fonts, false).is_err() {
        return false;
    }
    let Some(fonts) = fonts else { return false };
    let mut index = 0;
    let mut exists = BOOL(0);
    fonts.FindFamilyName(family, &mut index, &mut exists).is_ok() && exists.as_bool()
}

/// Premultiplied BGRA pixelekből 32 bites HICON (az ikon alfája nem premultiplied).
unsafe fn to_hicon(px: &[u8], size: u32) -> Result<HICON> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32), // felülről lefelé
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
    let dst = std::slice::from_raw_parts_mut(bits as *mut u8, px.len());
    for (d, s) in dst.chunks_exact_mut(4).zip(px.chunks_exact(4)) {
        let a = s[3] as u32;
        if a == 0 {
            d.fill(0);
            continue;
        }
        for i in 0..3 {
            d[i] = ((s[i] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
        d[3] = s[3];
    }
    let mask_bits = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
    let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
    let info = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&info);
    let _ = DeleteObject(color);
    let _ = DeleteObject(mask);
    icon
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChargeState;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

    fn dev(kind: DeviceKind, level: Option<u8>, charge: ChargeState, connected: bool) -> DeviceState {
        DeviceState { id: "test".into(), name: "Teszt".into(), kind, level, charge, connected }
    }

    const STATES: &[fn() -> DeviceState] = &[
        || dev(DeviceKind::Keyboard, Some(72), ChargeState::Discharging, true),
        || dev(DeviceKind::Keyboard, Some(35), ChargeState::Discharging, true),
        || dev(DeviceKind::Keyboard, Some(9), ChargeState::Discharging, true),
        || dev(DeviceKind::Keyboard, Some(52), ChargeState::Charging, true),
        || dev(DeviceKind::Keyboard, None, ChargeState::Charging, true),
        || dev(DeviceKind::Keyboard, None, ChargeState::Unknown, true),
        || dev(DeviceKind::Keyboard, Some(100), ChargeState::Full, true),
        || dev(DeviceKind::Keyboard, None, ChargeState::Unknown, false),
        || dev(DeviceKind::Mouse, Some(64), ChargeState::Discharging, true),
        || dev(DeviceKind::Headset, Some(46), ChargeState::Charging, true),
        || dev(DeviceKind::Controller, Some(12), ChargeState::Discharging, true),
        || dev(DeviceKind::Other, Some(88), ChargeState::Discharging, true),
    ];

    /// Előnézeti kép: minden tesztállapot, sötét és világos tálcán, 16/20/24/32 px-en, 4x nagyítva.
    /// Kimenet: target/icon-preview.png (`cargo test preview -- --nocapture`).
    #[test]
    fn preview_sheet() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let r = IconRenderer::new().unwrap();
            let states = STATES;
            let sizes = [16u32, 20, 24, 32];
            let themes = [(Theme { light: false }, 0x1C1C1C), (Theme { light: true }, 0xEEEEEE)];
            let (zoom, cell) = (4.0, 144.0);
            let (w, h) = ((sizes.len() * themes.len()) as u32 * cell as u32, states.len() as u32 * cell as u32);

            let sheet = r.wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand).unwrap();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                ..Default::default()
            };
            let rt = r.d2d.CreateWicBitmapRenderTarget(&sheet, &props).unwrap();
            rt.BeginDraw();
            for (ti, &(theme, bg)) in themes.iter().enumerate() {
                let x0 = (ti * sizes.len()) as f32 * cell;
                let bg = rt.CreateSolidColorBrush(&rgb(bg, 1.0), None).unwrap();
                rt.FillRectangle(&D2D_RECT_F { left: x0, top: 0.0, right: x0 + cell * sizes.len() as f32, bottom: h as f32 }, &bg);
                for (si, make) in states.iter().enumerate() {
                    let dev = make();
                    for (zi, &size) in sizes.iter().enumerate() {
                        let px = r.render_pixels(&dev, size, theme).unwrap();
                        let bmp_props = D2D1_BITMAP_PROPERTIES { pixelFormat: props.pixelFormat, dpiX: 96.0, dpiY: 96.0 };
                        let bmp = rt
                            .CreateBitmap(D2D_SIZE_U { width: size, height: size }, Some(px.as_ptr() as *const c_void), size * 4, &bmp_props)
                            .unwrap();
                        let d = size as f32 * zoom;
                        let (x, y) = (x0 + zi as f32 * cell + (cell - d) / 2.0, si as f32 * cell + (cell - d) / 2.0);
                        let dest = D2D_RECT_F { left: x, top: y, right: x + d, bottom: y + d };
                        rt.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                    }
                }
            }
            rt.EndDraw(None, None).unwrap();
            save_png(&r.wic, &sheet, concat!(env!("CARGO_MANIFEST_DIR"), "\\target\\icon-preview.png"));
        }
    }

    /// Elrendezés-változatok összehasonlítása 16 px-en, sötét tálcán: soronként egy változat,
    /// balra valódi méretben (1:1), jobbra 4x nagyítva. Kimenet: target/icon-variants.png.
    #[test]
    fn variants_sheet() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let mut r = IconRenderer::new().unwrap();
            let variants = [("Gyűrű", Layout::Ring), ("Torta", Layout::Pie), ("Sáv", Layout::Bar)];
            let devices = [STATES[4], STATES[8], STATES[9], STATES[10], STATES[5], STATES[7]];
            let theme = Theme { light: false };
            let (size, zoom) = (16u32, 4.0f32);
            let n = devices.len() as f32;
            let (row_h, label_w) = (96.0f32, 76.0);
            let (strip_w, gap) = (label_w + n * 32.0, 32.0);
            let w = (strip_w + gap + n * (size as f32 * zoom + 16.0)) as u32;
            let h = (variants.len() as f32 * row_h) as u32;

            let sheet = r.wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand).unwrap();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                ..Default::default()
            };
            let rt = r.d2d.CreateWicBitmapRenderTarget(&sheet, &props).unwrap();
            let format = r
                .dwrite
                .CreateTextFormat(w!("Segoe UI"), None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 18.0, w!(""))
                .unwrap();
            let mut pixels = Vec::new();
            for (_, layout) in &variants {
                r.tray_layout = *layout;
                pixels.push(devices.iter().map(|make| r.render_pixels(&make(), size, theme).unwrap()).collect::<Vec<_>>());
            }
            rt.BeginDraw();
            let bg = rt.CreateSolidColorBrush(&rgb(0x1C1C1C, 1.0), None).unwrap();
            rt.FillRectangle(&D2D_RECT_F { left: 0.0, top: 0.0, right: w as f32, bottom: h as f32 }, &bg);
            let fg = rt.CreateSolidColorBrush(&rgb(0xF2F2F3, 1.0), None).unwrap();
            for (vi, (label, _)) in variants.iter().enumerate() {
                let y0 = vi as f32 * row_h;
                let label: Vec<u16> = label.encode_utf16().collect();
                let rect = D2D_RECT_F { left: 12.0, top: y0 + 34.0, right: label_w, bottom: y0 + 60.0 };
                rt.DrawText(&label, &format, &rect, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                for (di, px) in pixels[vi].iter().enumerate() {
                    let bmp_props = D2D1_BITMAP_PROPERTIES { pixelFormat: props.pixelFormat, dpiX: 96.0, dpiY: 96.0 };
                    let bmp = rt
                        .CreateBitmap(D2D_SIZE_U { width: size, height: size }, Some(px.as_ptr() as *const c_void), size * 4, &bmp_props)
                        .unwrap();
                    // 1:1, a Windows 11 tálca kb. 32 px-es ikonhelyeivel.
                    let x = label_w + di as f32 * 32.0 + 8.0;
                    let y = y0 + (row_h - size as f32) / 2.0;
                    let dest = D2D_RECT_F { left: x, top: y, right: x + size as f32, bottom: y + size as f32 };
                    rt.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                    // 4x nagyítás.
                    let d = size as f32 * zoom;
                    let x = strip_w + gap + di as f32 * (d + 16.0);
                    let y = y0 + (row_h - d) / 2.0;
                    let dest = D2D_RECT_F { left: x, top: y, right: x + d, bottom: y + d };
                    rt.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                }
            }
            rt.EndDraw(None, None).unwrap();
            save_png(&r.wic, &sheet, concat!(env!("CARGO_MANIFEST_DIR"), "\\target\\icon-variants.png"));
        }
    }

    /// Az alkalmazás ikonja (exe, telepítő): a design logója, egy ~70%-os kiemelő színű gyűrű.
    /// Újragenerálás: `cargo test app_icon -- --ignored`, kimenet: res/napcat.ico.
    #[test]
    #[ignore = "felülírja a res/napcat.ico fájlt"]
    fn app_icon() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let r = IconRenderer::new().unwrap();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                ..Default::default()
            };
            let mut images = Vec::new();
            for size in [16u32, 20, 24, 32, 40, 48, 64, 256] {
                let bmp = r.wic.CreateBitmap(size, size, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand).unwrap();
                let rt: ID2D1DeviceContext = r.d2d.CreateWicBitmapRenderTarget(&bmp, &props).unwrap().cast().unwrap();
                let s = size as f32;
                let sw = s * 7.0 / 48.0;
                let radius = s / 2.0 - s * 1.5 / 48.0 - sw / 2.0;
                rt.BeginDraw();
                rt.Clear(Some(&rgb(0, 0.0)));
                let track = rt.CreateSolidColorBrush(&rgb(0x8A8A93, 0.35), None).unwrap();
                let ellipse = D2D1_ELLIPSE { point: D2D_POINT_2F { x: s / 2.0, y: s / 2.0 }, radiusX: radius, radiusY: radius };
                rt.DrawEllipse(&ellipse, &track, sw, None);
                let accent = rt.CreateSolidColorBrush(&rgb(0x4CC2FF, 1.0), None).unwrap();
                r.arc(&rt, s / 2.0, radius, sw, 0.7, &accent).unwrap();
                rt.EndDraw(None, None).unwrap();
                let path = format!("{}\\target\\app-icon-{size}.png", env!("CARGO_MANIFEST_DIR"));
                save_png(&r.wic, &bmp, &path);
                images.push((size, std::fs::read(&path).unwrap()));
            }
            std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "\\res\\napcat.ico"), ico(&images)).unwrap();
        }
    }

    /// ICO fájl PNG-tömörített képekből (Vista óta minden méretnél támogatott).
    fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&[0, 0, 1, 0]);
        out.extend_from_slice(&(images.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * images.len() as u32;
        for (size, png) in images {
            let dim = if *size >= 256 { 0 } else { *size as u8 };
            out.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
            out.extend_from_slice(&(png.len() as u32).to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            offset += png.len() as u32;
        }
        for (_, png) in images {
            out.extend_from_slice(png);
        }
        out
    }

    unsafe fn save_png(wic: &IWICImagingFactory, bmp: &IWICBitmap, path: &str) {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let stream = wic.CreateStream().unwrap();
        stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0).unwrap();
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
        enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let (mut frame, mut bag) = (None, None);
        enc.CreateNewFrame(&mut frame, &mut bag).unwrap();
        let frame = frame.unwrap();
        frame.Initialize(bag.as_ref()).unwrap();
        let (mut w, mut h) = (0, 0);
        bmp.GetSize(&mut w, &mut h).unwrap();
        frame.SetSize(w, h).unwrap();
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut fmt).unwrap();
        frame.WriteSource(bmp, std::ptr::null()).unwrap();
        frame.Commit().unwrap();
        enc.Commit().unwrap();
    }
}
