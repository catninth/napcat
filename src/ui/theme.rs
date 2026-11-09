//! Világos/sötét tálca és a gyűrű színei (a design paletta alapján).

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::w;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::System::Registry::*;

use crate::model::DeviceState;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub light: bool,
}

impl Theme {
    /// A tálca témája: HKCU\…\Themes\Personalize\SystemUsesLightTheme (alapból sötét).
    pub fn current() -> Theme {
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        let err = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("SystemUsesLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut value as *mut u32 as *mut c_void),
                Some(&mut size),
            )
        };
        Theme { light: err.is_ok() && value != 0 }
    }

    /// A tálca szövegszíne: a glyph és a háttérgyűrű ebből készül.
    pub fn fg(&self, alpha: f32) -> D2D1_COLOR_F {
        if self.light { rgb(0x17171A, alpha) } else { rgb(0xF2F2F3, alpha) }
    }

    pub fn track(&self) -> D2D1_COLOR_F {
        self.fg(if self.light { 0.14 } else { 0.18 })
    }

    /// Gyűrű szín: 50% felett zöld, 20–49% borostyán, 20% alatt piros, töltés kék,
    /// nincs kapcsolat vagy ismeretlen szint szürke.
    pub fn ring(&self, dev: &DeviceState) -> D2D1_COLOR_F {
        let (green, amber, red, blue, muted) = if self.light {
            (0x16A34A, 0xB45309, 0xDC2626, 0x2563EB, 0xA1A1AA)
        } else {
            (0x4ADE80, 0xFBBF24, 0xF87171, 0x60A5FA, 0x71717A)
        };
        let hex = match (dev.connected, dev.is_charging(), dev.level) {
            (false, _, _) => muted,
            (true, true, _) => blue,
            (true, false, None) => muted,
            (true, false, Some(l)) if l < 20 => red,
            (true, false, Some(l)) if l < 50 => amber,
            _ => green,
        };
        rgb(hex, 1.0)
    }
}

pub fn rgb(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xFF) as f32 / 255.0,
        g: ((hex >> 8) & 0xFF) as f32 / 255.0,
        b: (hex & 0xFF) as f32 / 255.0,
        a,
    }
}
