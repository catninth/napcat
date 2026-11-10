//! Akku előzmények a Statisztika oldalhoz: a tálca minden szint- és állapotváltozást hozzáfűz a
//! %APPDATA%\NapCat\history.log fájlhoz, a Statisztika oldal ebből számol.
//!
//! Egy sor: `<unix mp> <kód> <szint|-> <eszköz id>`. Kód: `d` merül, `c` tölt, `f` feltöltve,
//! `u` ismeretlen, `x` nincs csatlakoztatva. A `<mp> B - *` sor szakadás (a NapCat indult, vagy a
//! gép alvó állapotba ment / felébredt): előtte és utána mért értékek között nem tudjuk, mi történt,
//! ezért a számítások ott megszakítják a szakaszt.
//!
//! Csak változáskor írunk, így egy eszköz néhány száz sort ad naponta (kb. 1 MB évente).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use crate::model::{ChargeState, DeviceState};

/// Ennél nagyobb napló induláskor a két évnél régebbi soraitól megszabadul.
const PRUNE_BYTES: u64 = 4 << 20;
const KEEP_SECS: i64 = 2 * 365 * 86400;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sample {
    pub t: i64,
    /// 0-100; `NONE` ha nem ismert.
    pub level: u8,
    pub code: u8,
}

pub const NONE: u8 = 255;

impl Sample {
    /// Csatlakoztatott, ismert szintű minta (a grafikon pontja).
    pub fn known(&self) -> bool {
        self.level != NONE && self.code != b'x' && self.code != b'B'
    }
}

thread_local! {
    /// Az utoljára naplózott állapot eszközönként (kód, szint), hogy csak változást írjunk.
    static LOGGED: RefCell<HashMap<String, (u8, u8)>> = RefCell::new(HashMap::new());
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("NapCat").join("history.log"))
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn append(line: &str) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Induláskor: a túl nagy napló rövidítése, és szakadás (a leállás óta nem tudjuk, mi történt).
pub fn start() {
    if let Some(path) = path() {
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > PRUNE_BYTES) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let cut = now() - KEEP_SECS;
                let kept: String = text
                    .lines()
                    .filter(|l| l.split(' ').next().and_then(|t| t.parse::<i64>().ok()).is_some_and(|t| t >= cut))
                    .flat_map(|l| [l, "\n"])
                    .collect();
                let _ = std::fs::write(&path, kept);
            }
        }
    }
    gap();
}

/// Szakadás a naplóban (indulás, alvás, ébredés); utána minden eszköz állapota újra beíródik.
pub fn gap() {
    LOGGED.with_borrow_mut(|m| m.clear());
    append(&format!("{} B - *\n", now()));
}

fn code(d: &DeviceState) -> u8 {
    if !d.connected {
        return b'x';
    }
    match d.charge {
        ChargeState::Discharging => b'd',
        ChargeState::Charging => b'c',
        ChargeState::Full => b'f',
        ChargeState::Unknown => b'u',
    }
}

/// Egy eszköz friss állapota: ha változott a naplózotthoz képest, új sor. Szint nélküli eszközök
/// (pl. a csak csatlakozást mutató, kézzel hozzáadottak) nem kerülnek a naplóba.
pub fn record(d: &DeviceState) {
    let entry = (code(d), d.level.map_or(NONE, |l| l.min(100)));
    let id = d.id.replace(['\n', '\r'], "");
    let write = LOGGED.with_borrow_mut(|m| {
        let prev = m.get(&id).copied();
        if prev == Some(entry) || (prev.is_none() && entry.1 == NONE) {
            return false;
        }
        m.insert(id.clone(), entry);
        true
    });
    if write {
        let level = if entry.1 == NONE { "-".to_string() } else { entry.1.to_string() };
        append(&format!("{} {} {} {}\n", now(), entry.0 as char, level, id));
    }
}

/// Az egész napló eszközönként, időrendben; a szakadások minden addig látott eszközhöz bekerülnek.
pub fn load() -> HashMap<String, Vec<Sample>> {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn parse(text: &str) -> HashMap<String, Vec<Sample>> {
    let mut out: HashMap<String, Vec<Sample>> = HashMap::new();
    for line in text.lines() {
        let mut parts = line.splitn(4, ' ');
        let (Some(t), Some(code), Some(level), Some(id)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { continue };
        let Ok(t) = t.parse::<i64>() else { continue };
        let code = code.bytes().next().unwrap_or(b'u');
        if code == b'B' {
            for v in out.values_mut() {
                if v.last().is_some_and(|s| s.code != b'B') {
                    v.push(Sample { t, level: NONE, code });
                }
            }
            continue;
        }
        let level = level.parse::<u8>().map_or(NONE, |l| l.min(100));
        out.entry(id.to_string()).or_default().push(Sample { t, level, code });
    }
    out
}

/// Egy összefüggő merülési vagy töltési szakasz: az első és az utolsó szintváltozás között.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Run {
    pub start: i64,
    pub end: i64,
    pub from: u8,
    pub to: u8,
    pub charging: bool,
}

impl Run {
    pub fn hours(&self) -> f32 {
        (self.end - self.start) as f32 / 3600.0
    }

    /// Merülésnél a fogyás, töltésnél a növekedés (százalékpont; ellenkező irányban 0).
    pub fn delta(&self) -> f32 {
        let d = self.to as f32 - self.from as f32;
        (if self.charging { d } else { -d }).max(0.0)
    }
}

/// Merülő (vagy ismeretlen állapotú) minta: töltőn nincs. Feltöltve állapotban nem merül.
fn kind(s: &Sample) -> Option<bool> {
    if !s.known() {
        return None;
    }
    match s.code {
        b'd' | b'u' => Some(false),
        b'c' => Some(true),
        _ => None,
    }
}

/// A szakaszok: szakadás, leválasztás vagy állapotváltás zárja őket.
pub fn runs(samples: &[Sample]) -> Vec<Run> {
    let mut out = Vec::new();
    let mut cur: Option<(bool, Sample, Sample)> = None;
    let close = |cur: &mut Option<(bool, Sample, Sample)>, out: &mut Vec<Run>| {
        if let Some((charging, a, b)) = cur.take() {
            if b.t > a.t {
                out.push(Run { start: a.t, end: b.t, from: a.level, to: b.level, charging });
            }
        }
    };
    for s in samples {
        match kind(s) {
            None => close(&mut cur, &mut out),
            Some(k) => match &mut cur {
                Some((ck, _, last)) if *ck == k => *last = *s,
                _ => {
                    close(&mut cur, &mut out);
                    cur = Some((k, *s, *s));
                }
            },
        }
    }
    close(&mut cur, &mut out);
    out
}

/// Átlagos sebesség (%/óra) a `since` után végződő merülési vagy töltési szakaszokból; None, ha
/// kevés az adat.
pub fn rate(runs: &[Run], since: i64, charging: bool) -> Option<f32> {
    let (mut delta, mut hours) = (0.0, 0.0);
    for r in runs.iter().filter(|r| r.charging == charging && r.end > since) {
        delta += r.delta();
        hours += r.hours();
    }
    (hours >= 0.5 && delta >= 3.0).then(|| delta / hours)
}

/// Egyenértékű teljes ciklusok: az összes merülés / 100%.
pub fn cycles(runs: &[Run]) -> f32 {
    runs.iter().filter(|r| !r.charging).map(Run::delta).sum::<f32>() / 100.0
}

/// Napi merülés (százalékpont) a `day_starts` napokra (helyi éjfél, unix mp; a következő nap
/// kezdete a vége). A szomszédos merülő minták közti fogyás a későbbi minta napjához tartozik.
/// A második érték: volt-e aznap adat.
pub fn daily(samples: &[Sample], day_starts: &[i64]) -> Vec<(f32, bool)> {
    let mut out = vec![(0.0f32, false); day_starts.len()];
    let day_of = |t: i64| {
        let i = day_starts.partition_point(|&d| d <= t);
        (i > 0 && t < day_starts[i - 1] + 86400).then(|| i - 1)
    };
    for pair in samples.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if let Some(i) = day_of(b.t) {
            if b.known() {
                out[i].1 = true;
            }
            if kind(&a) == Some(false) && kind(&b) == Some(false) {
                out[i].0 += a.level as f32 - b.level as f32;
            }
        }
    }
    if let Some(i) = samples.first().filter(|s| s.known()).and_then(|s| day_of(s.t)) {
        out[i].1 = true;
    }
    for d in out.iter_mut() {
        d.0 = d.0.max(0.0);
    }
    out
}

/// A grafikon vonalai a [from, to] időszakra: összefüggő, ismert szintű szakaszok (idő, szint).
/// Az időszak előtti utolsó ismert szint a `from`-tól indul; ha a napló ismert szinttel ér
/// véget, az a `to`-ig tart (azóta nem változott).
pub fn segments(samples: &[Sample], from: i64, to: i64) -> Vec<Vec<(i64, u8)>> {
    let mut out = Vec::new();
    let mut cur: Vec<(i64, u8)> = Vec::new();
    let mut prev: Option<Sample> = None;
    for s in samples {
        if s.t > to {
            break;
        }
        if s.t >= from {
            if cur.is_empty() {
                if let Some(p) = prev.filter(|p| p.known() && p.t < from) {
                    cur.push((from, p.level));
                }
            }
            if s.known() {
                cur.push((s.t, s.level));
            } else if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        prev = Some(*s);
    }
    if cur.is_empty() {
        if let Some(p) = prev.filter(|p| p.known() && p.t < from) {
            cur.push((from, p.level));
        }
    }
    if let Some(&(_, level)) = cur.last() {
        if prev.is_some_and(|p| p.known()) {
            cur.push((to, level));
        }
        out.push(cur);
    }
    out
}

/// A szint `t`-kor a szakaszokból (két minta között egyenesen), ha van adat.
pub fn level_at(segments: &[Vec<(i64, u8)>], t: i64) -> Option<f32> {
    let seg = segments.iter().find(|s| s.first().is_some_and(|f| f.0 <= t) && s.last().is_some_and(|l| l.0 >= t))?;
    let i = seg.partition_point(|p| p.0 <= t);
    let a = seg[i.saturating_sub(1)];
    let Some(&b) = seg.get(i) else { return Some(a.1 as f32) };
    let k = if b.0 > a.0 { (t - a.0) as f32 / (b.0 - a.0) as f32 } else { 0.0 };
    Some(a.1 as f32 + (b.1 as f32 - a.1 as f32) * k)
}

/// Becsült kapacitás havonta: a merülés sebessége az első értékelhető hónaphoz képest.
#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    /// (év, hónap 1-12, becsült kapacitás %) az első adat hónapjától a mostaniig.
    pub months: Vec<(i32, u32, Option<f32>)>,
    /// A mostani becslés, None amíg nincs két értékelhető hónap.
    pub current: Option<f32>,
    /// Romlás havonta (százalékpont), ha legalább három értékelhető hónap van.
    pub per_month: Option<f32>,
}

/// Egy hónap akkor értékelhető, ha volt benne elég merülés.
const MONTH_HOURS: f32 = 4.0;
const MONTH_DROP: f32 = 20.0;

pub fn health(runs: &[Run], first: i64, now: i64, offset: i64) -> Health {
    let month_of = |t: i64| {
        let (y, m, _) = civil((t + offset).div_euclid(86400));
        (y, m)
    };
    let (fy, fm) = month_of(first);
    let (ny, nm) = month_of(now);
    let count = ((ny - fy) * 12 + nm as i32 - fm as i32).max(0) as usize + 1;
    let mut sums = vec![(0.0f32, 0.0f32); count];
    for r in runs.iter().filter(|r| !r.charging) {
        let (y, m) = month_of(r.start);
        let i = (y - fy) * 12 + m as i32 - fm as i32;
        if let Some(s) = usize::try_from(i).ok().and_then(|i| sums.get_mut(i)) {
            s.0 += r.delta();
            s.1 += r.hours();
        }
    }
    let rates: Vec<Option<f32>> = sums.iter().map(|&(d, h)| (h >= MONTH_HOURS && d >= MONTH_DROP).then(|| d / h)).collect();
    let base = rates.iter().flatten().next().copied();
    let months: Vec<(i32, u32, Option<f32>)> = rates
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let m0 = fm as i32 - 1 + i as i32;
            let v = r.zip(base).map(|(r, b)| (b / r * 100.0).clamp(50.0, 100.0));
            (fy + m0.div_euclid(12), m0.rem_euclid(12) as u32 + 1, v)
        })
        .collect();
    let valid: Vec<(f32, f32)> = months.iter().enumerate().filter_map(|(i, m)| m.2.map(|v| (i as f32, v))).collect();
    // A mostani érték az utolsó értékelhető hónapé (a grafikon vonala is ott ér véget).
    let current = (valid.len() >= 2).then(|| valid[valid.len() - 1].1);
    let per_month = (valid.len() >= 3).then(|| {
        let n = valid.len() as f32;
        let (mx, my) = (valid.iter().map(|v| v.0).sum::<f32>() / n, valid.iter().map(|v| v.1).sum::<f32>() / n);
        let cov: f32 = valid.iter().map(|v| (v.0 - mx) * (v.1 - my)).sum();
        let var: f32 = valid.iter().map(|v| (v.0 - mx).powi(2)).sum();
        if var > 0.0 { (-cov / var).max(0.0) } else { 0.0 }
    });
    Health { months, current, per_month }
}

/// A helyi idő eltérése az UTC-től másodpercben (a mostani, nyári időszámítással együtt).
pub fn local_offset() -> i64 {
    use windows::Win32::Foundation::FILETIME;
    unsafe {
        let utc = windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime();
        let mut local = FILETIME::default();
        if windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime(&utc, &mut local).is_err() {
            return 0;
        }
        let ticks = |f: FILETIME| ((f.dwHighDateTime as i64) << 32) | f.dwLowDateTime as i64;
        (ticks(local) - ticks(utc)) / 10_000_000
    }
}

/// Napok száma 1970-01-01 óta -> (év, hónap 1-12, nap 1-31).
pub fn civil(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + (m <= 2) as i64) as i32;
    (y, m, d)
}

/// A hét napja (0 = vasárnap) a napok számából.
pub fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: i64, level: u8, code: u8) -> Sample {
        Sample { t, level, code }
    }

    #[test]
    fn parses_and_breaks() {
        let log = "100 d 80 mouse 1\n200 d 79 mouse 1\n250 B - *\n300 c 50 kb\n400 x - mouse 1\nrossz sor\n";
        let m = parse(log);
        assert_eq!(m["mouse 1"], vec![s(100, 80, b'd'), s(200, 79, b'd'), s(250, NONE, b'B'), s(400, NONE, b'x')]);
        assert_eq!(m["kb"], vec![s(300, 50, b'c')]);
    }

    #[test]
    fn runs_and_rates() {
        let h = 3600;
        let samples = [
            s(0, 90, b'd'),
            s(h, 85, b'd'),
            s(2 * h, 80, b'd'),
            s(2 * h + 10, NONE, b'B'),
            s(5 * h, 70, b'd'),
            s(6 * h, 72, b'c'),
            s(7 * h, 92, b'c'),
            s(8 * h, 100, b'f'),
        ];
        let r = runs(&samples);
        // A szakadás utáni egyetlen merülő minta (70%) még nem szakasz.
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].from, r[0].to, r[0].charging), (90, 80, false));
        assert_eq!(rate(&r, 0, false), Some(5.0));
        assert_eq!(rate(&r, 0, true), Some(20.0));
        assert!((cycles(&r) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn daily_drain() {
        let d = 86400;
        let samples = [s(10, 90, b'd'), s(d - 10, 70, b'd'), s(d + 10, 71, b'd'), s(d + 20, 60, b'd'), s(3 * d, 60, b'x')];
        let days = daily(&samples, &[0, d, 2 * d]);
        assert_eq!(days, vec![(20.0, true), (10.0, true), (0.0, false)]);
    }

    #[test]
    fn health_by_month() {
        // 2026. jan. 1. 00:00 UTC; havonta egy 10 órás merülés, egyre gyorsabb.
        let jan = days_from(2026, 1, 1) * 86400;
        let month = 31 * 86400;
        let runs: Vec<Run> = (0..4)
            .map(|i| {
                let drop = [50, 52, 55, 60][i];
                Run { start: jan + i as i64 * month, end: jan + i as i64 * month + 36000, from: 100, to: 100 - drop, charging: false }
            })
            .collect();
        let h = health(&runs, jan, jan + 3 * month + 100, 0);
        assert_eq!(h.months.len(), 4);
        assert_eq!(h.months[0], (2026, 1, Some(100.0)));
        assert_eq!((h.months[3].0, h.months[3].1), (2026, 4));
        let cur = h.current.unwrap();
        assert!((cur - 83.33).abs() < 0.1, "{cur}");
        assert!(h.per_month.unwrap() > 1.0);
        // Egy hónapnyi adatból még nincs becslés.
        assert_eq!(health(&runs[..1], jan, jan + 100, 0).current, None);
    }

    fn days_from(y: i32, m: u32, d: u32) -> i64 {
        (0..40000).find(|&n| civil(n) == (y, m, d)).unwrap()
    }

    #[test]
    fn chart_segments() {
        let samples = [s(50, 90, b'd'), s(150, 80, b'd'), s(200, NONE, b'B'), s(300, 70, b'd'), s(400, 60, b'd')];
        let seg = segments(&samples, 100, 500);
        assert_eq!(seg, vec![vec![(100, 90), (150, 80)], vec![(300, 70), (400, 60), (500, 60)]]);
        assert_eq!(level_at(&seg, 350), Some(65.0));
        assert_eq!(level_at(&seg, 250), None);
        // Csak az időszak előtti adat: vízszintes vonal.
        assert_eq!(segments(&samples[3..], 450, 500), vec![vec![(450, 60), (500, 60)]]);
    }

    #[test]
    fn calendar() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_767), (2026, 11, 10));
        assert_eq!(weekday(20_767), 2); // kedd
    }
}
