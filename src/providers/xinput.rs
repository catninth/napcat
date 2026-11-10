//! Xbox (XInput) kontrollerek: XInputGetBatteryInformation. A Windows csak négy szintet ad
//! (üres / alacsony / közepes / teli), ezért a százalék közelítő.

use std::time::Duration;

use windows::Win32::UI::Input::XboxController::*;

use super::Provider;
use crate::model::{ChargeState, DeviceKind, DeviceState};

#[derive(Default)]
pub struct XInput {
    /// Mely játékos-helyeken (0-3) láttunk már vezeték nélküli kontrollert.
    seen: [bool; 4],
    connected: usize,
}

fn name(slot: u32) -> String {
    format!("{} {}", crate::i18n::t("Xbox kontroller", "Xbox controller"), slot + 1)
}

impl Provider for XInput {
    fn scan(&mut self) -> Vec<DeviceState> {
        let mut out = Vec::new();
        self.connected = 0;
        for slot in 0..4u32 {
            let id = format!("xinput-{slot}");
            let mut caps = XINPUT_CAPABILITIES::default();
            let present = unsafe { XInputGetCapabilities(slot, XINPUT_FLAG(0), &mut caps) } == 0;
            let mut info = XINPUT_BATTERY_INFORMATION::default();
            let battery = present && unsafe { XInputGetBatteryInformation(slot, BATTERY_DEVTYPE_GAMEPAD, &mut info) } == 0;
            // Csak akkus (vezeték nélküli) kontroller érdekes; a vezetékes nem jelent akkut.
            if battery && info.BatteryType != BATTERY_TYPE_DISCONNECTED && info.BatteryType != BATTERY_TYPE_WIRED {
                let level = match info.BatteryLevel {
                    BATTERY_LEVEL_EMPTY => 5,
                    BATTERY_LEVEL_LOW => 25,
                    BATTERY_LEVEL_MEDIUM => 60,
                    _ => 100,
                };
                self.seen[slot as usize] = true;
                self.connected += 1;
                out.push(DeviceState {
                    id,
                    name: name(slot),
                    kind: DeviceKind::Controller,
                    level: Some(level),
                    level_approx: true,
                    charge: ChargeState::Discharging,
                    connected: true,
                    hw: None, estimate: None,
                });
            } else if self.seen[slot as usize] {
                out.push(DeviceState::disconnected(&id, &name(slot), DeviceKind::Controller));
            }
        }
        out
    }

    fn poll_interval(&self) -> Option<Duration> {
        (self.connected > 0).then_some(Duration::from_secs(60))
    }
}
