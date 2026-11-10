//! Világos/sötét tálca és a gyűrű színei (a design paletta alapján).

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::System::Registry::*;

use crate::model::DeviceState;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub light: bool,
}

/// A beállításokból: az alkalmazás ablakainak témája (0 rendszer, 1 sötét, 2 világos), a kiemelő
/// szín (0: a Windows-é, 1: semleges, különben 0x01RRGGBB egyéni), és hogy a "rendben" állapotú
/// gyűrű a kiemelő színt kapja-e. A tálca ikonjai mindig a tálca témáját követik.
static UI_THEME: AtomicU8 = AtomicU8::new(0);
static ACCENT: AtomicU32 = AtomicU32::new(0);
static RING_ACCENT: AtomicBool = AtomicBool::new(false);

pub fn configure(ui_theme: &str, use_accent: bool, custom: Option<u32>, ring_accent: bool) {
    UI_THEME.store(
        match ui_theme {
            "dark" => 1,
            "light" => 2,
            _ => 0,
        },
        Ordering::Relaxed,
    );
    let accent = match (use_accent, custom) {
        (false, _) => 1,
        (true, Some(c)) => 0x0100_0000 | (c & 0xFF_FFFF),
        (true, None) => 0,
    };
    ACCENT.store(accent, Ordering::Relaxed);
    RING_ACCENT.store(use_accent && ring_accent, Ordering::Relaxed);
}

fn personalize(value: PCWSTR) -> bool {
    let mut v = 0u32;
    let mut size = size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    err.is_ok() && v != 0
}

/// A Windows kiemelő színe a téma szerinti árnyalatban (sötét témán a világosabb "Light2",
/// világoson a sötétebb "Dark1", ahogy a Windows saját felületei is); alapból a design kékje.
pub fn windows_accent(light: bool) -> u32 {
    let mut palette = [0u8; 32];
    let mut size = palette.len() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(palette.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if err.is_err() || size < 32 {
        return if light { 0x005FB8 } else { 0x4CC2FF };
    }
    let i = if light { 4 } else { 1 } * 4;
    (palette[i] as u32) << 16 | (palette[i + 1] as u32) << 8 | palette[i + 2] as u32
}

impl Theme {
    /// A tálca témája: HKCU\…\Themes\Personalize\SystemUsesLightTheme (alapból sötét).
    pub fn current() -> Theme {
        Theme { light: personalize(w!("SystemUsesLightTheme")) }
    }

    /// Az alkalmazás ablakainak (panel, beállítások) témája: a beállítás, vagy "rendszer" esetén
    /// a tálcáé, hogy a panel illeszkedjen hozzá.
    pub fn ui() -> Theme {
        match UI_THEME.load(Ordering::Relaxed) {
            1 => Theme { light: false },
            2 => Theme { light: true },
            _ => Theme::current(),
        }
    }

    /// A design palettájának többi színe (felugró panel, beállítások).
    pub fn tile(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xEEEFF2, 1.0) } else { rgb(0x2A2B30, 1.0) }
    }

    pub fn border(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0x000000, 0.09) } else { rgb(0xFFFFFF, 0.08) }
    }

    pub fn red(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xDC2626, 1.0) } else { rgb(0xF87171, 1.0) }
    }

    /// Figyelmeztető borostyán (pl. a 80%-os kapacitás vonala a Statisztikán).
    pub fn amber(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xB45309, 1.0) } else { rgb(0xFBBF24, 1.0) }
    }

    pub fn alert_bg(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xDC2626, 0.08) } else { rgb(0xF87171, 0.12) }
    }

    /// Kiemelő színnel kitöltött gomb szövege.
    pub fn on_accent(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xFFFFFF, 1.0) } else { rgb(0x0B0B0C, 1.0) }
    }

    pub fn accent(&self) -> D2D1_COLOR_F {
        match ACCENT.load(Ordering::Relaxed) {
            0 => rgb(windows_accent(self.light), 1.0),
            1 => if self.light { rgb(0x3F3F46, 1.0) } else { rgb(0xE4E4E7, 1.0) }, // semleges
            c => rgb(c & 0xFF_FFFF, 1.0),
        }
    }

    // A beállítások ablak színei (design: háttér, sorok, szegmensválasztó).
    pub fn window(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xF3F3F5, 1.0) } else { rgb(0x1C1D21, 1.0) }
    }

    pub fn row(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xFFFFFF, 1.0) } else { rgb(0x26272C, 1.0) }
    }

    pub fn segment_selected(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xE4E4E9, 1.0) } else { rgb(0x3A3B41, 1.0) }
    }

    pub fn control_border(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0x8B8B94, 1.0) } else { rgb(0x8B8B94, 1.0) }
    }

    pub fn knob_off(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0x52525B, 1.0) } else { rgb(0xC8C8CF, 1.0) }
    }

    /// A tálca szövegszíne: a glyph és a háttérgyűrű ebből készül.
    pub fn fg(&self, alpha: f32) -> D2D1_COLOR_F {
        if self.light { rgb(0x17171A, alpha) } else { rgb(0xF2F2F3, alpha) }
    }

    /// A felugró panel háttere, fő és másodlagos szövegszíne.
    pub fn panel(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0xFAFAFB, 1.0) } else { rgb(0x1F2024, 1.0) }
    }

    pub fn sub(&self) -> D2D1_COLOR_F {
        if self.light { rgb(0x52525B, 1.0) } else { rgb(0xA3A3AD, 1.0) }
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
            _ if RING_ACCENT.load(Ordering::Relaxed) => return self.accent(),
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
