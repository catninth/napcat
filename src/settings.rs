//! Beállítások: %APPDATA%\NapCat\config.toml, és az automatikus indítás (HKCU Run kulcs).
//!
//! A fájl a TOML egy egyszerű részhalmaza (`kulcs = érték` sorok), ezért nem kell hozzá
//! külön TOML/serde függőség. Ismeretlen kulcsokat és hibás sorokat kihagy.

use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// A tálcán elrejtett eszközök azonosítói.
    pub hidden: Vec<String>,
    /// Értesítés 15%-nál és 5%-nál.
    pub notify_low: bool,
    /// A tálcaikon elrendezése: "ring", "pie" vagy "bar".
    pub icon_style: String,
    /// Frissítések: "auto" (letölt és telepít), "notify" (csak szól) vagy "off".
    pub update_mode: String,
    /// Az első indításkori tipp (ikonok kitűzése) már megjelent.
    pub pin_tip_shown: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            hidden: Vec::new(),
            notify_low: true,
            icon_style: "ring".into(),
            update_mode: "notify".into(),
            pin_tip_shown: false,
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("NapCat").join("config.toml"))
}

impl Settings {
    pub fn load() -> Settings {
        let mut s = Settings::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return s };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "notify_low" => s.notify_low = value != "false",
                "icon_style" => s.icon_style = value.trim_matches('"').to_string(),
                "update_mode" => s.update_mode = value.trim_matches('"').to_string(),
                "pin_tip_shown" => s.pin_tip_shown = value == "true",
                "hidden" => {
                    s.hidden = value
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .split(',')
                        .map(|v| v.trim().trim_matches('"').to_string())
                        .filter(|v| !v.is_empty())
                        .collect()
                }
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let hidden: Vec<String> = self.hidden.iter().map(|h| format!("\"{}\"", h.replace('"', ""))).collect();
        let text = format!(
            "# NapCat beállítások\nnotify_low = {}\nicon_style = \"{}\"\nupdate_mode = \"{}\"\npin_tip_shown = {}\nhidden = [{}]\n",
            self.notify_low,
            self.icon_style.replace('"', ""),
            self.update_mode.replace('"', ""),
            self.pin_tip_shown,
            hidden.join(", ")
        );
        let _ = std::fs::write(path, text);
    }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

/// Ugyanazt az értéket használja, mint a telepítő "Indítás a Windows-zal" jelölőnégyzete.
pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("NapCat"), RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if !on {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("NapCat"));
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(Some(0)).collect();
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            w!("NapCat"),
            REG_SZ.0,
            Some(value.as_ptr() as *const c_void),
            (value.len() * 2) as u32,
        );
    }
}
