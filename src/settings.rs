//! Beállítások: %APPDATA%\NapCat\config.toml, és az automatikus indítás (HKCU Run kulcs).
//!
//! A fájl a TOML egy egyszerű részhalmaza (`kulcs = érték` sorok), ezért nem kell hozzá
//! külön TOML/serde függőség. Ismeretlen kulcsokat és hibás sorokat kihagy.

use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::*;

/// Kézzel hozzáadott eszköz: a NapCat nem ismeri a protokollját, ezért csak a csatlakozást mutatja.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomDevice {
    pub vid: u16,
    pub pid: u16,
    /// A HID termék neve (ugyanaz a VID/PID több módot is jelölhet, pl. kábel és dongle).
    pub product: String,
    /// "keyboard", "mouse", "headset", "controller" vagy "other".
    pub kind: String,
}

impl CustomDevice {
    pub fn id(&self) -> String {
        format!("custom-{:04x}-{:04x}-{}", self.vid, self.pid, self.product.to_lowercase().replace(' ', "-"))
    }

    fn encode(&self) -> String {
        format!("{:04x}|{:04x}|{}|{}", self.vid, self.pid, self.product.replace(['|', '"'], ""), self.kind)
    }

    fn decode(s: &str) -> Option<CustomDevice> {
        let mut p = s.split('|');
        let vid = u16::from_str_radix(p.next()?, 16).ok()?;
        let pid = u16::from_str_radix(p.next()?, 16).ok()?;
        Some(CustomDevice { vid, pid, product: p.next()?.to_string(), kind: p.next().unwrap_or("other").to_string() })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// A tálcán elrejtett eszközök azonosítói (a panelen és a beállításokban látszanak).
    pub hidden: Vec<String>,
    /// Az Eszközök oldalon eltávolított eszközök: sehol nem látszanak, amíg vissza nem adod őket.
    pub removed: Vec<String>,
    /// Kézzel hozzáadott, nem támogatott eszközök (csak csatlakozás).
    pub custom: Vec<CustomDevice>,
    /// Lemerülés figyelmeztetés a küszöbnél (és 5%-nál).
    pub notify_low: bool,
    /// A lemerülés figyelmeztetés küszöbe (%), 5-30.
    pub low_threshold: u8,
    /// Értesítés teli töltésnél.
    pub notify_full: bool,
    /// Teljes képernyős alkalmazás (játék, film) közben az értesítések várnak.
    pub quiet_fullscreen: bool,
    /// Tálca mód: "each" (eszközönként egy ikon) vagy "one" (egy ikon a legsürgősebb eszközzel).
    pub tray_mode: String,
    /// A tálcaikon elrendezése: "ring", "pie" vagy "bar".
    pub icon_style: String,
    /// Az alkalmazás ablakainak témája: "system", "dark" vagy "light".
    pub theme: String,
    /// Kiemelő szín használata (különben semleges fehér-szürke).
    pub use_accent: bool,
    /// Egyéni kiemelő szín (0xRRGGBB), None: a Windows kiemelő színe.
    pub custom_accent: Option<u32>,
    /// A "rendben" állapotú (50% feletti) gyűrű a kiemelő színt kapja a zöld helyett.
    pub ring_accent: bool,
    /// Frissítési gyakoriság másodpercben (30, 60, 300); a providerek időközei ehhez arányosak.
    pub poll_secs: u32,
    /// Frissítések: "auto" (letölt és telepít), "notify" (csak szól) vagy "off".
    pub update_mode: String,
    /// Az első indításkori tipp (ikonok kitűzése) már megjelent.
    pub pin_tip_shown: bool,
    /// A felugró panel rács nézetben (különben lista).
    pub flyout_grid: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            hidden: Vec::new(),
            removed: Vec::new(),
            custom: Vec::new(),
            notify_low: true,
            low_threshold: 15,
            notify_full: true,
            quiet_fullscreen: true,
            tray_mode: "each".into(),
            icon_style: "ring".into(),
            theme: "system".into(),
            use_accent: true,
            custom_accent: None,
            ring_accent: false,
            poll_secs: 60,
            update_mode: "notify".into(),
            pin_tip_shown: false,
            flyout_grid: false,
        }
    }
}

/// `["a", "b"]` -> ["a", "b"]
fn parse_list(value: &str) -> Vec<String> {
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_device_roundtrip() {
        let c = CustomDevice { vid: 0x3142, pid: 0x0a80, product: "fifine Microphone".into(), kind: "headset".into() };
        assert_eq!(CustomDevice::decode(&c.encode()), Some(c.clone()));
        assert_eq!(parse_list(&format!("[\"{}\", \"x\"]", c.encode())).len(), 2);
        assert_eq!(c.id(), "custom-3142-0a80-fifine-microphone");
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
            let text = value.trim_matches('"').to_string();
            match key.trim() {
                "notify_low" => s.notify_low = value != "false",
                "low_threshold" => s.low_threshold = value.parse::<u8>().map_or(15, |v| v.clamp(5, 30)),
                "notify_full" => s.notify_full = value != "false",
                "quiet_fullscreen" => s.quiet_fullscreen = value != "false",
                "tray_mode" => s.tray_mode = text,
                "icon_style" => s.icon_style = text,
                "theme" => s.theme = text,
                "use_accent" => s.use_accent = value != "false",
                "custom_accent" => s.custom_accent = u32::from_str_radix(text.trim_start_matches('#'), 16).ok(),
                "ring_accent" => s.ring_accent = value == "true",
                "poll_secs" => s.poll_secs = value.parse::<u32>().map_or(60, |v| v.clamp(30, 300)),
                "update_mode" => s.update_mode = text,
                "pin_tip_shown" => s.pin_tip_shown = value == "true",
                "flyout_grid" => s.flyout_grid = value == "true",
                "hidden" => s.hidden = parse_list(value),
                "removed" => s.removed = parse_list(value),
                "custom" => s.custom = parse_list(value).iter().filter_map(|v| CustomDevice::decode(v)).collect(),
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
        let list = |items: Vec<String>| items.iter().map(|h| format!("\"{}\"", h.replace('"', ""))).collect::<Vec<_>>().join(", ");
        let hidden = list(self.hidden.clone());
        let removed = list(self.removed.clone());
        let custom_devices = list(self.custom.iter().map(CustomDevice::encode).collect());
        let q = |s: &str| format!("\"{}\"", s.replace('"', ""));
        let accent = self.custom_accent.map_or("\"\"".to_string(), |c| format!("\"#{c:06X}\""));
        let text = format!(
            "# NapCat beállítások\n\
             notify_low = {}\nlow_threshold = {}\nnotify_full = {}\nquiet_fullscreen = {}\n\
             tray_mode = {}\nicon_style = {}\ntheme = {}\nuse_accent = {}\ncustom_accent = {}\nring_accent = {}\n\
             poll_secs = {}\nupdate_mode = {}\npin_tip_shown = {}\nflyout_grid = {}\nhidden = [{}]\nremoved = [{}]\ncustom = [{}]\n",
            self.notify_low,
            self.low_threshold,
            self.notify_full,
            self.quiet_fullscreen,
            q(&self.tray_mode),
            q(&self.icon_style),
            q(&self.theme),
            self.use_accent,
            accent,
            self.ring_accent,
            self.poll_secs,
            q(&self.update_mode),
            self.pin_tip_shown,
            self.flyout_grid,
            hidden,
            removed,
            custom_devices
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
