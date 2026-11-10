//! Központi típusok, amelyeket a providerek adnak és a UI megjelenít.

// Egyes változatokat csak a későbbi providerek (BT, HID++…) állítanak elő.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Headset,
    Controller,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChargeState {
    Discharging,
    Charging,
    Full,
    Unknown,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeviceState {
    pub id: String, // stabil azonosító (pl. BT cím, HID path hash)
    pub name: String,
    pub kind: DeviceKind,
    pub level: Option<u8>, // 0-100, None ha ismeretlen
    /// A szint csak közelítő (pl. XInput: négy fokozat), "~72%"-ként jelenik meg.
    pub level_approx: bool,
    pub charge: ChargeState,
    pub connected: bool,
    /// USB/HID azonosító (VID, PID), ha ismert: ebből tudja az Eszközök oldal, hogy a Windows által
    /// észlelt eszközök közül melyik szerepel már a NapCat-ben.
    pub hw: Option<(u16, u16)>,
    /// A tálca számolja az előzményekből (a providerek nem töltik ki): pl. "kb. 40 perc",
    /// "1 ó 10 p a telig", leválasztva "2 órája".
    pub estimate: Option<String>,
}

impl DeviceState {
    /// Egy korábban látott, most nem csatlakoztatott eszköz.
    pub fn disconnected(id: &str, name: &str, kind: DeviceKind) -> DeviceState {
        DeviceState {
            id: id.into(),
            name: name.into(),
            kind,
            level: None,
            level_approx: false,
            charge: ChargeState::Unknown,
            connected: false,
            hw: None,
            estimate: None,
        }
    }

    pub fn is_charging(&self) -> bool {
        self.connected && self.charge == ChargeState::Charging
    }

    /// A szint szövegként, pl. "72%" vagy közelítő értéknél "~60%".
    pub fn level_text(&self) -> Option<String> {
        let l = self.level?;
        Some(if self.level_approx { format!("~{l}%") } else { format!("{l}%") })
    }

    /// Tooltip szöveg, pl. "Logitech G305 – 72% – nem tölt".
    pub fn tooltip(&self) -> String {
        use crate::i18n::t;
        if !self.connected {
            return format!("{} – {}", self.name, t("nincs csatlakoztatva", "not connected"));
        }
        let level = self.level_text().unwrap_or_else(|| t("ismeretlen szint", "level unknown").to_string());
        let charge = match self.charge {
            ChargeState::Discharging => t("nem tölt", "not charging"),
            ChargeState::Charging => t("tölt", "charging"),
            ChargeState::Full => t("feltöltve", "fully charged"),
            ChargeState::Unknown => return format!("{} – {level}", self.name),
        };
        format!("{} – {level} – {charge}", self.name)
    }
}
