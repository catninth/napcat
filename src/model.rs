//! Központi típusok, amelyeket a providerek adnak és a UI megjelenít.

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

#[derive(Clone, Debug)]
#[allow(dead_code)] // az id-t az eszközszál fogja használni
pub struct DeviceState {
    pub id: String, // stabil azonosító (pl. BT cím, HID path hash)
    pub name: String,
    pub kind: DeviceKind,
    pub level: Option<u8>, // 0-100, None ha ismeretlen
    pub charge: ChargeState,
    pub connected: bool,
}

impl DeviceState {
    pub fn is_charging(&self) -> bool {
        self.connected && self.charge == ChargeState::Charging
    }

    /// Tooltip szöveg, pl. "Logitech G305 – 72% – nem tölt".
    pub fn tooltip(&self) -> String {
        if !self.connected {
            return format!("{} – nincs csatlakoztatva", self.name);
        }
        let level = self.level.map_or("ismeretlen szint".to_string(), |l| format!("{l}%"));
        let charge = match self.charge {
            ChargeState::Discharging => "nem tölt",
            ChargeState::Charging => "tölt",
            ChargeState::Full => "feltöltve",
            ChargeState::Unknown => return format!("{} – {level}", self.name),
        };
        format!("{} – {level} – {charge}", self.name)
    }
}
