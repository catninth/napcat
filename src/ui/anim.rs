//! Animációk az azonnali módú (minden képkockában újrarajzolt) felületekhez.
//!
//! A vezérlők rajzoláskor egy kulccsal és a célértékkel kérnek egy értéket; ha a cél megváltozott,
//! az érték az előzőből simán (ease-out) fut át. Amíg bármelyik mozog, `active()` igaz, és az
//! ablak a következő képkockát kéri (időzítő). Új kulcs első kérésre animáció nélkül a célon áll.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

/// Képkocka időköz az animációs időzítőhöz (ms).
pub const FRAME_MS: u32 = 15;

struct Anim {
    from: f32,
    to: f32,
    start: Instant,
    dur: Duration,
}

impl Anim {
    fn value(&self, now: Instant) -> (f32, bool) {
        if self.from == self.to {
            return (self.to, false);
        }
        let t = (now.saturating_duration_since(self.start).as_secs_f32() / self.dur.as_secs_f32().max(0.001)).min(1.0);
        let e = 1.0 - (1.0 - t).powi(3);
        (self.from + (self.to - self.from) * e, t < 1.0)
    }
}

#[derive(Default)]
pub struct Anims {
    map: HashMap<String, Anim>,
    active: bool,
    now: Option<Instant>,
}

impl Anims {
    /// Egy képkocka kezdete: az idő rögzítése, az aktív jelző törlése.
    pub fn frame(&mut self) {
        self.now = Some(Instant::now());
        self.active = false;
    }

    fn now(&self) -> Instant {
        self.now.unwrap_or_else(Instant::now)
    }

    /// Az érték `target` felé `ms` alatt; a cél változásakor a jelenlegi értékből indul.
    pub fn get(&mut self, key: &str, target: f32, ms: u64) -> f32 {
        let now = self.now();
        let dur = Duration::from_millis(ms);
        let a = self.map.entry(key.to_string()).or_insert(Anim { from: target, to: target, start: now, dur });
        if a.to != target {
            let (current, _) = a.value(now);
            *a = Anim { from: current, to: target, start: now, dur };
        }
        let (v, running) = a.value(now);
        self.active |= running;
        v
    }

    /// Az érték azonnali beállítása (pl. húzás közben a kurzorhoz), animáció nélkül.
    pub fn set(&mut self, key: &str, value: f32) {
        let now = self.now();
        self.map.insert(key.to_string(), Anim { from: value, to: value, start: now, dur: Duration::ZERO });
    }

    /// Kell-e még képkocka.
    pub fn active(&self) -> bool {
        self.active
    }
}

/// Két szín keveréke (t = 0: a, t = 1: b).
pub fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    let l = |x: f32, y: f32| x + (y - x) * t;
    D2D1_COLOR_F { r: l(a.r, b.r), g: l(a.g, b.g), b: l(a.b, b.b), a: l(a.a, b.a) }
}

/// A szín átlátszósága `k`-szorosra.
pub fn fade(c: D2D1_COLOR_F, k: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * k, ..c }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_to_target() {
        let mut a = Anims::default();
        a.frame();
        assert_eq!(a.get("x", 1.0, 100), 1.0);
        assert!(!a.active());
        a.frame();
        let v = a.get("x", 0.0, 100);
        assert!(v > 0.99 && a.active());
        std::thread::sleep(Duration::from_millis(120));
        a.frame();
        assert_eq!(a.get("x", 0.0, 100), 0.0);
        assert!(!a.active());
    }
}
