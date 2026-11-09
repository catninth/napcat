//! Becslés az eszközök előzményeiből (a design szerint): "kb. 40 perc" (merülés),
//! "1 ó 10 p a telig" (töltés), leválasztott eszköznél "2 órája".
//!
//! Eszközönként a szint változásait jegyezzük fel; töltésállapot-váltáskor elölről kezdjük.
//! Csak elég hosszú (5 perc) és elég nagy (5%) változásból becslünk, mert sok eszköz 10%-os
//! lépésekben jelent, és rövid idő alatt értelmetlen számok jönnének ki.

use std::time::Instant;

use crate::model::{ChargeState, DeviceState};

const MIN_SECS: f32 = 300.0;
const MIN_STEP: i32 = 5;

#[derive(Default)]
pub struct History {
    /// (idő, szint) a jelenlegi merülési vagy töltési szakaszban.
    samples: Vec<(Instant, u8)>,
    charging: bool,
    /// Mikor volt utoljára csatlakoztatva.
    last_seen: Option<Instant>,
}

impl History {
    pub fn record(&mut self, d: &DeviceState, now: Instant) {
        if !d.connected {
            return;
        }
        self.last_seen = Some(now);
        let charging = d.charge == ChargeState::Charging;
        if charging != self.charging {
            self.samples.clear();
            self.charging = charging;
        }
        if let Some(level) = d.level {
            if self.samples.last().map_or(true, |s| s.1 != level) {
                self.samples.push((now, level));
                if self.samples.len() > 64 {
                    self.samples.remove(0);
                }
            }
        }
    }

    pub fn estimate(&self, d: &DeviceState, now: Instant) -> Option<String> {
        if !d.connected {
            return self.last_seen.map(|t| ago(now.duration_since(t).as_secs()));
        }
        let level = d.level? as f32;
        let (&(t0, l0), &(t1, l1)) = (self.samples.first()?, self.samples.last()?);
        let secs = t1.duration_since(t0).as_secs_f32();
        let delta = l1 as i32 - l0 as i32;
        if secs < MIN_SECS || delta.abs() < MIN_STEP {
            return None;
        }
        let rate = delta.abs() as f32 / secs; // % / mp
        match (d.charge, delta < 0) {
            (ChargeState::Discharging, true) => Some(format!("kb. {}", duration(level / rate))),
            (ChargeState::Charging, false) if level < 100.0 => Some(format!("{} a telig", short_duration((100.0 - level) / rate))),
            _ => None,
        }
    }
}

/// "40 perc", "3 óra", "4 nap".
fn duration(secs: f32) -> String {
    let mins = (secs / 60.0).round() as u64;
    match mins {
        ..=59 => format!("{} perc", mins.max(1)),
        60..=1439 => format!("{} óra", (mins as f32 / 60.0).round() as u64),
        _ => format!("{} nap", (mins as f32 / 1440.0).round() as u64),
    }
}

/// "1 ó 10 p", "35 p".
fn short_duration(secs: f32) -> String {
    let mins = (secs / 60.0).round() as u64;
    match (mins / 60, mins % 60) {
        (0, m) => format!("{} p", m.max(1)),
        (h, 0) => format!("{h} ó"),
        (h, m) => format!("{h} ó {m} p"),
    }
}

/// "most", "5 perce", "2 órája", "3 napja".
fn ago(secs: u64) -> String {
    match secs {
        ..=59 => "most".into(),
        60..=3599 => format!("{} perce", secs / 60),
        3600..=86399 => format!("{} órája", secs / 3600),
        _ => format!("{} napja", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DeviceKind;
    use std::time::Duration;

    fn dev(level: u8, charge: ChargeState) -> DeviceState {
        DeviceState { level: Some(level), charge, connected: true, ..DeviceState::disconnected("x", "x", DeviceKind::Mouse) }
    }

    #[test]
    fn discharge_and_charge_estimates() {
        let t = Instant::now();
        let mut h = History::default();
        h.record(&dev(60, ChargeState::Discharging), t);
        // 10% esés 1 óra alatt -> 50%-ból még 5 óra.
        h.record(&dev(50, ChargeState::Discharging), t + Duration::from_secs(3600));
        assert_eq!(h.estimate(&dev(50, ChargeState::Discharging), t + Duration::from_secs(3600)).as_deref(), Some("kb. 5 óra"));

        // Töltés: új szakasz; 20% 30 perc alatt -> 70%-ról 30% hiányzik: 45 perc.
        h.record(&dev(50, ChargeState::Charging), t + Duration::from_secs(3700));
        assert_eq!(h.estimate(&dev(50, ChargeState::Charging), t + Duration::from_secs(3700)), None);
        h.record(&dev(70, ChargeState::Charging), t + Duration::from_secs(3700 + 1800));
        assert_eq!(h.estimate(&dev(70, ChargeState::Charging), t + Duration::from_secs(5500)).as_deref(), Some("45 p a telig"));

        // Leválasztva: mióta.
        let off = DeviceState::disconnected("x", "x", DeviceKind::Mouse);
        assert_eq!(h.estimate(&off, t + Duration::from_secs(5500 + 7200)).as_deref(), Some("2 órája"));
    }

    #[test]
    fn formats() {
        assert_eq!(duration(40.0 * 60.0), "40 perc");
        assert_eq!(duration(4.0 * 86400.0), "4 nap");
        assert_eq!(short_duration(70.0 * 60.0), "1 ó 10 p");
        assert_eq!(ago(30), "most");
    }
}
