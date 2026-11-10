//! Közös Direct2D rajzoló segédek a felugró panelhez és a beállítások ablakhoz (DIP-ben).

use windows::core::{w, Result};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;

use super::glyphs;
use super::icon_render::{AppIcon, IconRenderer};
use super::theme::Theme;

/// A design ikonjai (24-es rács, körvonalas rajz).
pub mod icon {
    pub const GRID: &str = "M5.5 4h4A1.5 1.5 0 0 1 11 5.5v4A1.5 1.5 0 0 1 9.5 11h-4A1.5 1.5 0 0 1 4 9.5v-4A1.5 1.5 0 0 1 5.5 4zM14.5 4h4A1.5 1.5 0 0 1 20 5.5v4A1.5 1.5 0 0 1 18.5 11h-4A1.5 1.5 0 0 1 13 9.5v-4A1.5 1.5 0 0 1 14.5 4zM5.5 13h4A1.5 1.5 0 0 1 11 14.5v4A1.5 1.5 0 0 1 9.5 20h-4A1.5 1.5 0 0 1 4 18.5v-4A1.5 1.5 0 0 1 5.5 13zM14.5 13h4A1.5 1.5 0 0 1 20 14.5v4A1.5 1.5 0 0 1 18.5 20h-4A1.5 1.5 0 0 1 13 18.5v-4A1.5 1.5 0 0 1 14.5 13z";
    pub const LIST: &str = "M4 6h16M4 12h16M4 18h16";
    pub const GEAR: &str = "M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z";
    pub const WARNING: &str = "M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z";
    pub const PLUS: &str = "M12 5v14M5 12h14";
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

pub fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub struct Painter<'a> {
    pub ctx: &'a ID2D1DeviceContext,
    pub renderer: &'a IconRenderer,
}

impl Painter<'_> {
    pub unsafe fn brush(&self, c: &D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        self.ctx.CreateSolidColorBrush(c, None)
    }

    pub unsafe fn fill(&self, r: D2D_RECT_F, radius: f32, c: D2D1_COLOR_F) -> Result<()> {
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
        self.ctx.FillRoundedRectangle(&rr, &self.brush(&c)?);
        Ok(())
    }

    pub unsafe fn stroke(&self, r: D2D_RECT_F, radius: f32, width: f32, c: D2D1_COLOR_F) -> Result<()> {
        let h = width / 2.0;
        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: r.left + h, top: r.top + h, right: r.right - h, bottom: r.bottom - h }, radiusX: radius, radiusY: radius };
        self.ctx.DrawRoundedRectangle(&rr, &self.brush(&c)?, width, None);
        Ok(())
    }

    pub unsafe fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: D2D1_COLOR_F) -> Result<()> {
        self.ctx.DrawLine(D2D_POINT_2F { x: x0, y: y0 }, D2D_POINT_2F { x: x1, y: y1 }, &self.brush(&c)?, 1.0, None);
        Ok(())
    }

    pub unsafe fn circle(&self, cx: f32, cy: f32, r: f32, c: D2D1_COLOR_F) -> Result<()> {
        let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: r, radiusY: r };
        self.ctx.FillEllipse(&e, &self.brush(&c)?);
        Ok(())
    }

    /// Egy design ikon `size` méretben, (x, y) bal felső sarokkal; `width` a vonalvastagság a 24-es rácson.
    pub unsafe fn icon(&self, d: &str, x: f32, y: f32, size: f32, width: f32, c: D2D1_COLOR_F) -> Result<()> {
        let k = size / 24.0;
        let geo = glyphs::geometry(self.renderer.d2d(), d, k, x, y)?;
        self.ctx.DrawGeometry(&geo, &self.brush(&c)?, width * k, &self.renderer.stroke_style(&[])?);
        Ok(())
    }

    /// A NapCat app ikon (alvó macska a gyűrűben) `size` méretben, (x, y) bal felső sarokkal. Ez a
    /// logó, ezért a design állapotai szerint (sötét, világos, feltöltve), nem a kiemelő színnel.
    pub unsafe fn logo(&self, x: f32, y: f32, size: f32) -> Result<()> {
        let style = AppIcon::current(Theme::ui().light);
        // Az ikon a (0, 0) ponttól rajzol: eltolással kerül a helyére (a görgetést megtartva).
        let mut old = windows::Foundation::Numerics::Matrix3x2::default();
        self.ctx.GetTransform(&mut old);
        let mut t = old;
        t.M31 += x;
        t.M32 += y;
        self.ctx.SetTransform(&t);
        let res = self.renderer.draw_app_icon(self.ctx, size, style);
        self.ctx.SetTransform(&old);
        res
    }

    /// Egysoros szöveg a téglalapban, függőlegesen középre; túl hosszúnál "…".
    pub unsafe fn text(&self, s: &str, size: f32, weight: DWRITE_FONT_WEIGHT, r: D2D_RECT_F, c: D2D1_COLOR_F, align: DWRITE_TEXT_ALIGNMENT) -> Result<()> {
        let dwrite = self.renderer.dwrite();
        let format = dwrite.CreateTextFormat(w!("Segoe UI Variable Text"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!(""))?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        format.SetTextAlignment(align)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let ellipsis = dwrite.CreateEllipsisTrimmingSign(&format)?;
        let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        format.SetTrimming(&trimming, &ellipsis)?;
        let wide: Vec<u16> = s.encode_utf16().collect();
        self.ctx.DrawText(&wide, &format, &r, &self.brush(&c)?, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        Ok(())
    }

    /// A szöveg szélessége (gombok méretezéséhez).
    pub unsafe fn text_width(&self, s: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<f32> {
        let dwrite = self.renderer.dwrite();
        let format = dwrite.CreateTextFormat(w!("Segoe UI Variable Text"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!(""))?;
        let wide: Vec<u16> = s.encode_utf16().collect();
        let layout = dwrite.CreateTextLayout(&wide, &format, 10_000.0, 100.0)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        Ok(m.widthIncludingTrailingWhitespace)
    }
}
