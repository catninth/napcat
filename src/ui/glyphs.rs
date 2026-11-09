//! A design eszköz-glyph-jei (24 egységes rácson, körvonalas rajz) Direct2D geometriaként.
//!
//! A Segoe Fluent ikonok vonalai 16 px-en 1 px alattiak; ezek a rajzok egyszerűbbek és a
//! vonalvastagságuk méretenként állítható, így a tálcán is jól olvashatók.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;

use crate::model::DeviceKind;

/// SVG útvonal (M, L, H, V, A, Z és kisbetűs változataik), a design Icons artboardjáról.
pub fn path(kind: DeviceKind) -> Option<&'static str> {
    Some(match kind {
        DeviceKind::Mouse => "M12 3a6 6 0 0 0-6 6v6a6 6 0 0 0 12 0V9a6 6 0 0 0-6-6zM12 7v3",
        DeviceKind::Keyboard => "M3 6.5h18v11H3zM7 10.5h.01M10.3 10.5h.01M13.7 10.5h.01M17 10.5h.01M8 14h8",
        DeviceKind::Headset => "M4 15v-3a8 8 0 0 1 16 0v3M4 14h3v6H5a1 1 0 0 1-1-1zM20 14h-3v6h2a1 1 0 0 0 1-1z",
        DeviceKind::Controller => {
            "M7 7.5h10a4 4 0 0 1 4 3.6l.5 5a2.6 2.6 0 0 1-4.7 1.7L15.5 16h-7l-1.3 1.8a2.6 2.6 0 0 1-4.7-1.7l.5-5A4 4 0 0 1 7 7.5zM7.5 10.5v3M6 12h3M15 11h.01M17 13h.01"
        }
        DeviceKind::Other => return None,
    })
}

#[derive(Clone, Copy)]
enum Tok {
    Cmd(char),
    Num(f32),
}

fn tokenize(d: &str) -> Vec<Tok> {
    let b = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() {
            out.push(Tok::Cmd(c));
            i += 1;
        } else if c == '-' || c == '.' || c.is_ascii_digit() {
            // Egy szám a következő előjelig vagy második tizedespontig tart ("0-6", ".5.5").
            let start = i;
            let mut dot = c == '.';
            i += 1;
            while i < b.len() {
                match b[i] as char {
                    ch if ch.is_ascii_digit() => i += 1,
                    '.' if !dot => {
                        dot = true;
                        i += 1;
                    }
                    _ => break,
                }
            }
            out.push(Tok::Num(d[start..i].parse().unwrap_or(0.0)));
        } else {
            i += 1;
        }
    }
    out
}

/// Geometria a `d` útvonalból: pont = (dx + x·scale, dy + y·scale).
pub unsafe fn geometry(factory: &ID2D1Factory1, d: &str, scale: f32, dx: f32, dy: f32) -> Result<ID2D1PathGeometry> {
    let geo = factory.CreatePathGeometry()?;
    let sink = geo.Open()?;
    let toks = tokenize(d);
    let pt = |x: f32, y: f32| D2D_POINT_2F { x: dx + x * scale, y: dy + y * scale };
    let num = |i: &mut usize| {
        let v = match toks.get(*i) {
            Some(Tok::Num(v)) => *v,
            _ => 0.0,
        };
        *i += 1;
        v
    };
    let (mut x, mut y, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut open = false;
    let mut cmd = 'M';
    let mut i = 0;
    while i < toks.len() {
        if let Tok::Cmd(c) = toks[i] {
            cmd = c;
            i += 1;
            if c.eq_ignore_ascii_case(&'z') {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
                (x, y) = (sx, sy);
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (bx, by) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            'M' => {
                (x, y) = (bx + num(&mut i), by + num(&mut i));
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
                sink.BeginFigure(pt(x, y), D2D1_FIGURE_BEGIN_HOLLOW);
                open = true;
                (sx, sy) = (x, y);
                cmd = if rel { 'l' } else { 'L' }; // további számpárok vonalak
            }
            'L' => {
                (x, y) = (bx + num(&mut i), by + num(&mut i));
                sink.AddLine(pt(x, y));
            }
            'H' => {
                x = bx + num(&mut i);
                sink.AddLine(pt(x, y));
            }
            'V' => {
                y = by + num(&mut i);
                sink.AddLine(pt(x, y));
            }
            'A' => {
                let (rx, ry, rot) = (num(&mut i), num(&mut i), num(&mut i));
                let (large, sweep) = (num(&mut i) != 0.0, num(&mut i) != 0.0);
                (x, y) = (bx + num(&mut i), by + num(&mut i));
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: pt(x, y),
                    size: D2D_SIZE_F { width: rx * scale, height: ry * scale },
                    rotationAngle: rot,
                    sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                    arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                });
            }
            _ => i += 1, // nem támogatott parancs: átlépjük
        }
    }
    if open {
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
    }
    sink.Close()?;
    Ok(geo.into())
}
