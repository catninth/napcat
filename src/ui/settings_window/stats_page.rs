//! A Statisztika oldal a design "Stats" artboardja szerint: eszközválasztó, összesítő kártyák,
//! töltöttség és napi merülés grafikon (7 vagy 30 nap), és az akkumulátor becsült állapota az
//! első adat óta. Az adatok a tálca előzménynaplójából jönnek (`crate::stats`).

use std::collections::HashMap;
use std::time::Instant;

use super::*;
use crate::stats::{self, Sample};

/// A betöltött előzmények és az időpont, amihez a grafikonok igazodnak.
pub struct Data {
    pub log: HashMap<String, Vec<Sample>>,
    pub loaded: Instant,
    /// Unix mp és a helyi idő eltérése.
    pub now: i64,
    pub offset: i64,
}

impl Data {
    pub fn load() -> Data {
        Data { log: stats::load(), loaded: Instant::now(), now: stats::now(), offset: stats::local_offset() }
    }
}

/// Az oldal választásai.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub dev: Option<String>,
    /// 7 vagy 30 nap.
    pub range: u32,
    pub table: bool,
}

impl Default for View {
    fn default() -> View {
        View { dev: None, range: 7, table: false }
    }
}

const PAD: f32 = 16.0;
/// A grafikonok bal szélén a tengelyfeliratok helye.
const AXIS: f32 = 34.0;

fn en() -> bool {
    crate::i18n::english()
}

/// Szám tizedesjeggyel, magyarul tizedesvesszővel.
fn num(v: f32, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if en() { s } else { s.replace('.', ",") }
}

fn month_name(m: u32) -> &'static str {
    const HU: [&str; 12] = ["jan.", "febr.", "márc.", "ápr.", "máj.", "jún.", "júl.", "aug.", "szept.", "okt.", "nov.", "dec."];
    const EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let i = (m.clamp(1, 12) - 1) as usize;
    if en() { EN[i] } else { HU[i] }
}

fn weekday_short(wd: u32) -> &'static str {
    const HU: [&str; 7] = ["V", "H", "K", "Sze", "Cs", "P", "Szo"];
    const EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    if en() { EN[wd as usize % 7] } else { HU[wd as usize % 7] }
}

fn weekday_long(wd: u32) -> &'static str {
    const HU: [&str; 7] = ["vasárnap", "hétfő", "kedd", "szerda", "csütörtök", "péntek", "szombat"];
    const EN: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    if en() { EN[wd as usize % 7] } else { HU[wd as usize % 7] }
}

/// "nov. 3." / "Nov 3"
fn day_month(day: i64) -> String {
    let (_, m, d) = stats::civil(day);
    if en() { format!("{} {d}", month_name(m)) } else { format!("{} {d}.", month_name(m)) }
}

/// "2026. márc. 14." / "Mar 14, 2026"
fn full_date(day: i64) -> String {
    let (y, m, d) = stats::civil(day);
    if en() { format!("{} {d}, {y}", month_name(m)) } else { format!("{y}. {} {d}.", month_name(m)) }
}

/// "2027. szept." / "Sep 2027"
fn year_month(y: i32, m: u32) -> String {
    if en() { format!("{} {y}", month_name(m)) } else { format!("{y}. {}", month_name(m)) }
}

/// Időtartam két részre bontva a kártyákhoz: ("1", "ó 30 p") / ("1", "h 30 min").
fn split_duration(hours: f32) -> (String, String) {
    let mins = (hours * 60.0).round().max(1.0) as u32;
    let (h, m) = (mins / 60, mins % 60);
    let (hu, mu) = if en() { ("h", "min") } else { ("ó", "p") };
    match (h, m) {
        (0, m) => (m.to_string(), mu.to_string()),
        (h, 0) => (h.to_string(), hu.to_string()),
        (h, m) => (h.to_string(), format!("{hu} {m:02} {mu}")),
    }
}

/// Egy összesítő kártya tartalma.
struct Kpi {
    label: &'static str,
    value: String,
    unit: String,
    sub: String,
}

impl Ui<'_> {
    /// Kártya háttér a teljes tartalomszélességben.
    unsafe fn card(&self, y: f32, h: f32) -> Result<()> {
        self.p.fill(rect(MARGIN, y, W - 2.0 * MARGIN, h), 10.0, self.theme.row())
    }

    /// Kártya fejléce: cím balra, jobbra igazított szöveg (pl. a rámutatott érték).
    unsafe fn card_head(&self, y: f32, title: &str, readout: &str) -> Result<()> {
        let x = MARGIN + PAD;
        self.p.text(title, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x, y, 260.0, 20.0), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.p.text(readout, 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 200.0, y, W - 2.0 * MARGIN - 2.0 * PAD - 200.0, 20.0), self.theme.sub(), DWRITE_TEXT_ALIGNMENT_TRAILING)
    }

    /// Vízszintes rácsvonal felirattal a bal tengelyen.
    unsafe fn grid_line(&self, y: f32, label: &str) -> Result<()> {
        let (l, r) = (MARGIN + PAD + AXIS, W - MARGIN - PAD);
        self.p.line(l, y + 0.5, r, y + 0.5, self.theme.fg(if self.theme.light { 0.09 } else { 0.07 }))?;
        self.p.text(label, 11.0, DWRITE_FONT_WEIGHT_NORMAL, rect(l - 46.0, y - 8.0, 40.0, 16.0), self.theme.sub(), DWRITE_TEXT_ALIGNMENT_TRAILING)
    }

    /// Tengelyfelirat középre `x`-en.
    unsafe fn tick(&self, x: f32, y: f32, label: &str) -> Result<()> {
        self.p.text(label, 11.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x - 40.0, y - 8.0, 80.0, 16.0), self.theme.sub(), DWRITE_TEXT_ALIGNMENT_CENTER)
    }

    /// A rámutatás x-e, ha a kurzor a zónában van; a zóna a mozgásra újrarajzol.
    fn zone(&mut self, r: D2D_RECT_F) -> Option<f32> {
        self.zones.push(r);
        self.hover.filter(|&(x, y)| contains(&r, x, y)).map(|(x, _)| x)
    }

    pub(super) unsafe fn build_stats(&mut self, s: &Settings, devices: &[DeviceState], data: &Data, view: &View) -> Result<f32> {
        let (fg, sub) = (self.theme.fg(1.0), self.theme.sub());
        let right = W - MARGIN;
        let mut y = 24.0;
        self.p.logo(MARGIN, y, 32.0)?;
        self.p.text(t("Statisztika", "Statistics"), 24.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(MARGIN + 44.0, y, 300.0, 32.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let range = if view.range == 30 { "30" } else { "7" };
        self.segmented_bg(right + 16.0, y + 16.0, self.theme.row(), &[("7", t("7 nap", "7 days")), ("30", t("30 nap", "30 days"))], range, |k| {
            Act::StatsRange(k.parse().unwrap_or(7))
        })?;
        y += 32.0 + 20.0;

        // Csak a szintet jelentő eszközök, amelyekről már van adat.
        let shown: Vec<&DeviceState> = devices
            .iter()
            .filter(|d| !s.removed.contains(&d.id) && data.log.get(&d.id).is_some_and(|v| v.iter().any(Sample::known)))
            .collect();
        let Some(&sel) = view.dev.as_ref().and_then(|id| shown.iter().find(|d| d.id == *id)).or(shown.first()) else {
            let msg = t(
                "A NapCat mostantól feljegyzi az eszközök töltöttségét. A grafikonok néhány óra használat után telnek meg, \
                 az akkumulátor állapotához pedig legalább két hónapnyi adat kell.",
                "NapCat now keeps a record of your devices' battery levels. The charts fill in after a few hours of use, \
                 and battery health needs at least two months of data.",
            );
            let h = self.p.paragraph_height(msg, 12.5, W - 2.0 * MARGIN - 2.0 * PAD)?;
            self.card(y, 16.0 + 20.0 + 8.0 + h + 16.0)?;
            self.p.text(t("Még nincs adat", "No data yet"), 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(MARGIN + PAD, y + 16.0, 400.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.p.paragraph(msg, 12.5, rect(MARGIN + PAD, y + 44.0, W - 2.0 * MARGIN - 2.0 * PAD, h), sub)?;
            return Ok(y + 60.0 + h + 32.0);
        };

        // Eszközválasztó fülek (tördelve).
        let mut x = MARGIN;
        for d in &shown {
            let tw = self.p.text_width(&d.name, 13.0, DWRITE_FONT_WEIGHT_MEDIUM)?;
            let w = 8.0 + 28.0 + 8.0 + tw + 14.0;
            if x + w > right && x > MARGIN {
                x = MARGIN;
                y += 44.0 + 6.0;
            }
            let r = rect(x, y, w, 44.0);
            let on = d.id == sel.id;
            let h = self.hover_anim(&format!("tab{}", d.id), &r);
            let bg = if on { self.theme.segment_selected() } else { fade(self.theme.fg(0.05), h) };
            self.p.fill(r, 8.0, bg)?;
            self.p.stroke(r, 8.0, 1.0, if on { self.theme.fg(0.18) } else { self.theme.border() })?;
            self.at(x + 8.0, y + 8.0);
            self.p.renderer.draw(self.p.ctx, d, 28.0, self.theme, Layout::Ring, Some(if on { bg } else { self.theme.window() }))?;
            self.reset();
            self.p.text(&d.name, 13.0, DWRITE_FONT_WEIGHT_MEDIUM, rect(x + 44.0, y, tw + 4.0, 44.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.hits.push((r, Act::StatsDevice(d.id.clone())));
            x += w + 6.0;
        }
        y += 44.0 + 20.0;

        // Az időszak: a mai nappal együtt `days` naptári nap (helyi idő szerint).
        let samples = &data.log[&sel.id];
        let runs = stats::runs(samples);
        let (now, off) = (data.now, data.offset);
        let today = (now + off).div_euclid(86400);
        let days = view.range.max(1) as i64;
        let day0 = today - days + 1;
        let day_starts: Vec<i64> = (0..days).map(|k| (day0 + k) * 86400 - off).collect();
        let from = day_starts[0];
        let span = (days * 86400) as f32;
        let daily = stats::daily(samples, &day_starts);
        let drain = stats::rate(&runs, from, false);
        let charge = stats::rate(&runs, now - 90 * 86400, true);
        let data_days = daily.iter().filter(|d| d.1).count();
        let avg_daily = (data_days > 0).then(|| daily.iter().map(|d| d.0).sum::<f32>() / data_days as f32).filter(|&v| v >= 0.5);
        let first = samples.iter().find(|s| s.known()).map_or(now, |s| s.t);

        // Összesítő kártyák.
        let dash = || "–".to_string();
        let kpis = [
            match drain {
                Some(r) => Kpi { label: t("Átlagos merülés", "Average drain"), value: num(r, if r < 10.0 { 1 } else { 0 }), unit: t("%/óra", "%/h").into(), sub: t("használat közben", "while in use").into() },
                None => Kpi { label: t("Átlagos merülés", "Average drain"), value: dash(), unit: String::new(), sub: t("még kevés a merülés", "not enough discharge yet").into() },
            },
            match drain {
                Some(r) => {
                    let hours = 100.0 / r;
                    let (value, unit) = if hours < 100.0 { (format!("{}", hours.round() as u32), t("óra", "hours")) } else { (num(hours / 24.0, 1), t("nap", "days")) };
                    let sub = match avg_daily {
                        Some(d) if en() => format!("about {} days at your usage", num(100.0 / d, 1)),
                        Some(d) => format!("kb. {} nap a szokásaid szerint", num(100.0 / d, 1)),
                        None => t("folyamatos használattal", "in continuous use").into(),
                    };
                    Kpi { label: t("Egy töltéssel", "On one charge"), value, unit: unit.into(), sub }
                }
                None => Kpi { label: t("Egy töltéssel", "On one charge"), value: dash(), unit: String::new(), sub: String::new() },
            },
            match charge {
                Some(r) => {
                    let (value, unit) = split_duration(85.0 / r);
                    Kpi { label: t("Teljes töltés", "Full charge"), value, unit, sub: t("15%-ról 100%-ra", "from 15% to 100%").into() }
                }
                None => Kpi { label: t("Teljes töltés", "Full charge"), value: dash(), unit: String::new(), sub: t("még nem láttam töltést", "no charge seen yet").into() },
            },
            {
                let c = stats::cycles(&runs);
                let since = t("az első indítás óta", "since first launch").to_string();
                Kpi { label: t("Töltési ciklusok", "Charge cycles"), value: num(c, if c < 10.0 { 1 } else { 0 }), unit: t("db", "").into(), sub: since }
            },
        ];
        let kw = (W - 2.0 * MARGIN - 3.0 * 8.0) / 4.0;
        for (i, k) in kpis.iter().enumerate() {
            let x = MARGIN + i as f32 * (kw + 8.0);
            self.p.fill(rect(x, y, kw, 100.0), 10.0, self.theme.row())?;
            self.p.text(k.label, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x + 14.0, y + 12.0, kw - 28.0, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            let vw = self.p.text_width(&k.value, 22.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            self.p.text(&k.value, 22.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x + 14.0, y + 32.0, vw + 2.0, 30.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.p.text(&k.unit, 13.0, DWRITE_FONT_WEIGHT_MEDIUM, rect(x + 14.0 + vw + 4.0, y + 36.0, kw - 32.0 - vw, 28.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            self.p.paragraph(&k.sub, 11.5, rect(x + 14.0, y + 66.0, kw - 28.0, 30.0), sub)?;
        }
        y += 100.0 + 20.0;

        let (l, r) = (MARGIN + PAD + AXIS, W - MARGIN - PAD);
        let px = |t: i64| l + (r - l) * ((t - from) as f32 / span).clamp(0.0, 1.0);
        let t_at = |x: f32| from + (((x - l) / (r - l)).clamp(0.0, 1.0) * span) as i64;
        // Napfeliratok: 7 napnál a hét napjai, 30 napnál hetente a dátum, a végén "ma".
        let day_label = |k: i64| {
            let day = day0 + k;
            if days <= 7 { weekday_short(stats::weekday(day)).to_string() } else if day == today { t("ma", "today").to_string() } else { day_month(day) }
        };
        let label_days: Vec<i64> = if days <= 7 { (0..days).collect() } else { (0..days).rev().step_by(7).collect() };
        let when = |t: i64| {
            let local = t + off;
            let (day, sec) = (local.div_euclid(86400), local.rem_euclid(86400));
            let clock = format!("{:02}:{:02}", sec / 3600, sec % 3600 / 60);
            if days <= 7 { format!("{} {clock}", weekday_long(stats::weekday(day))) } else { format!("{} {clock}", day_month(day)) }
        };

        // Töltöttség grafikon.
        let accent = self.accent;
        let top = y + 16.0 + 20.0 + 10.0;
        let (ct, cb) = (top + 12.0, top + 168.0);
        let py = |v: f32| cb - (cb - ct) * v / 100.0;
        let segs = stats::segments(samples, from, now.min(from + days * 86400));
        let hx = self.zone(rect(l, ct - 12.0, r - l, cb - ct + 24.0));
        let readout = match hx {
            Some(x) => {
                let t0 = t_at(x);
                match stats::level_at(&segs, t0) {
                    Some(v) => format!("{} · {}%", when(t0), v.round()),
                    None => format!("{} · {}", when(t0), t("nincs adat", "no data")),
                }
            }
            None => match (sel.connected, sel.level_text()) {
                (true, Some(lv)) => format!("{} {lv} · {}", t("Most:", "Now:"), t("vidd fölé az egeret a részletekért", "hover for details")),
                _ => t("Nincs csatlakoztatva", "Not connected").to_string(),
            },
        };
        self.card(y, 16.0 + 20.0 + 10.0 + 200.0 + 6.0)?;
        self.card_head(y + 16.0, t("Töltöttség", "Battery level"), &readout)?;
        for v in [0, 25, 50, 75, 100] {
            self.grid_line(py(v as f32), &format!("{v}%"))?;
        }
        if segs.is_empty() {
            self.p.text(t("Nincs adat ebben az időszakban", "No data in this period"), 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(l, ct, r - l, cb - ct), sub, DWRITE_TEXT_ALIGNMENT_CENTER)?;
        }
        for seg in &segs {
            let pts: Vec<(f32, f32)> = seg.iter().map(|&(t, v)| (px(t), py(v as f32))).collect();
            self.p.area(&pts, cb, fade(accent, 0.12))?;
            self.p.polyline(&pts, 2.0, accent, &[])?;
            if pts.len() == 1 {
                self.p.circle(pts[0].0, pts[0].1, 2.0, accent)?;
            }
        }
        for &k in &label_days {
            let x = (px(day_starts[k as usize]) + px(day_starts[k as usize] + 86400)) / 2.0;
            self.tick(x, top + 190.0, &day_label(k))?;
        }
        if let Some(x) = hx {
            self.p.line(x, ct, x, cb, self.theme.fg(0.3))?;
            if let Some(v) = stats::level_at(&segs, t_at(x)) {
                self.p.circle(x, py(v), 6.0, self.theme.row())?;
                self.p.circle(x, py(v), 4.5, accent)?;
            }
        }
        y += 16.0 + 20.0 + 10.0 + 200.0 + 6.0 + 20.0;

        // Napi merülés oszlopok.
        let top = y + 16.0 + 20.0 + 10.0;
        let (bt, bb) = (top + 8.0, top + 130.0);
        let max = daily.iter().map(|d| d.0).fold(0.0f32, f32::max);
        let bmax = ((max / 20.0).ceil() * 20.0).max(20.0);
        let slot = (r - l) / days as f32;
        let bw = (slot * if days <= 7 { 0.5 } else { 0.62 }).max(4.0);
        let hover_bar = self.zone(rect(l, bt, r - l, bb - bt + 24.0)).map(|x| (((x - l) / slot) as usize).min(days as usize - 1));
        let readout = match hover_bar {
            Some(k) => {
                let label = if days <= 7 { weekday_long(stats::weekday(day0 + k as i64)).to_string() } else { day_month(day0 + k as i64) };
                match daily[k] {
                    (v, true) if en() => format!("{label} · {}% used", v.round()),
                    (v, true) => format!("{label} · {}% merült", v.round()),
                    (_, false) => format!("{label} · {}", t("nincs adat", "no data")),
                }
            }
            None => match avg_daily {
                Some(v) if en() => format!("Average: {}% / day", v.round()),
                Some(v) => format!("Átlag: {}% / nap", v.round()),
                None => t("Még kevés az adat", "Not enough data yet").to_string(),
            },
        };
        self.card(y, 16.0 + 20.0 + 10.0 + 150.0 + 6.0)?;
        self.card_head(y + 16.0, t("Napi merülés", "Daily drain"), &readout)?;
        for v in [0.0, bmax / 2.0, bmax] {
            self.grid_line(bb - (bb - bt) * v / bmax, &format!("{}%", v.round()))?;
        }
        for (k, &(v, _)) in daily.iter().enumerate() {
            if v <= 0.0 {
                continue;
            }
            let h = (bb - bt) * v / bmax;
            let x = l + slot * k as f32 + (slot - bw) / 2.0;
            let alpha = if hover_bar.map_or(true, |h| h == k) { 1.0 } else { 0.4 };
            self.p.fill(rect(x, bb - h, bw, h), 3.0_f32.min(bw / 2.0), fade(accent, alpha))?;
        }
        for &k in &label_days {
            self.tick(l + slot * (k as f32 + 0.5), top + 145.0, &day_label(k))?;
        }
        y += 16.0 + 20.0 + 10.0 + 150.0 + 6.0 + 20.0;

        // Akkumulátor állapota az első adat óta.
        self.build_health(y, &runs, first, data, view)
    }

    unsafe fn build_health(&mut self, mut y: f32, runs: &[stats::Run], first: i64, data: &Data, view: &View) -> Result<f32> {
        let (fg, sub) = (self.theme.fg(1.0), self.theme.sub());
        let (now, off) = (data.now, data.offset);
        let h = stats::health(runs, first, now, off);
        let inner = W - 2.0 * MARGIN - 2.0 * PAD;
        let note = t(
            "Becslés: a merülés sebességéből számolva, az első értékelhető hónaphoz viszonyítva; a használat változása \
             (pl. világítás, frissítési gyakoriság) is beleszól. A legtöbb eszköz csak százalékot jelent, valódi kapacitást nem.",
            "Estimate: based on how fast the battery drains, compared with the first month with enough data; changes in \
             use (lighting, polling rate) also affect it. Most devices only report a percentage, not the real capacity.",
        );
        let note_h = self.p.paragraph_height(note, 11.5, inner)?;
        let valid = h.months.iter().filter(|m| m.2.is_some()).count();
        let body = match (h.current, view.table) {
            (None, _) => 0.0,
            (Some(_), false) => 190.0 + 14.0,
            (Some(_), true) => 28.0 + 24.0 * h.months.len() as f32 + 14.0,
        };
        let card_h = PAD + 36.0 + 14.0 + 48.0 + 14.0 + body + note_h + PAD;
        self.card(y, card_h)?;
        let x = MARGIN + PAD;
        let mut cy = y + PAD;
        self.p.text(t("Akkumulátor állapota", "Battery health"), 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x, cy, 300.0, 20.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let first_day = (first + off).div_euclid(86400);
        let since = format!("{} · {}", t("Első indítás óta", "Since first launch"), full_date(first_day));
        self.p.text(&since, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x, cy + 20.0, 400.0, 16.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        if h.current.is_some() {
            let label = if view.table { t("Grafikon", "Chart") } else { t("Táblázat", "Table") };
            // Körvonalas gomb (a design szerint), rámutatáskor halvány kitöltéssel.
            let bw = self.p.text_width(label, 12.0, DWRITE_FONT_WEIGHT_MEDIUM)? + 24.0;
            let r = rect(W - MARGIN - PAD - bw, cy, bw, 32.0);
            let hv = self.hover_anim("stats-table", &r);
            if hv > 0.0 {
                self.p.fill(r, 6.0, self.theme.fg(0.06 * hv))?;
            }
            self.p.stroke(r, 6.0, 1.0, self.theme.fg(0.14))?;
            self.p.text(label, 12.0, DWRITE_FONT_WEIGHT_MEDIUM, r, fg, DWRITE_TEXT_ALIGNMENT_CENTER)?;
            self.hits.push((r, Act::StatsTable));
        }
        cy += 36.0 + 14.0;

        // A nagy szám és a két sor mellette.
        let (big, line1, line2) = match h.current {
            Some(c) => {
                let lost = 100.0 - c;
                // A design szerint csak a szám félkövér.
                let line1 = (format!("−{}%", num(lost, 1)), t(" kapacitás az első indítás óta", " capacity since first launch").to_string());
                let line2 = match h.per_month {
                    Some(p) if p >= 0.05 => {
                        let months = ((c - 80.0).max(0.0) / p).round() as i32;
                        let (ny, nm, _) = stats::civil((now + off).div_euclid(86400));
                        let m0 = nm as i32 - 1 + months;
                        let eta = year_month(ny + m0.div_euclid(12), m0.rem_euclid(12) as u32 + 1);
                        if en() { format!("about {}% a month · around 80%: {eta}", num(p, 2)) } else { format!("kb. {}% havonta · 80% körül: {eta}", num(p, 2)) }
                    }
                    Some(_) => t("Nem látszik romlás", "No wear visible").to_string(),
                    None => t("A havi ütemhez legalább három hónap kell", "The monthly rate needs at least three months").to_string(),
                };
                (num(c, 1), line1, line2)
            }
            None => {
                let line2 = if en() {
                    format!("Needs two months with at least 4 hours and 20% of discharge each ({valid} so far)")
                } else {
                    format!("Két hónap kell, mindkettőben legalább 4 óra és 20% merüléssel (eddig {valid})")
                };
                ("–".to_string(), (t("Gyűjtöm az adatokat", "Collecting data").to_string(), String::new()), line2)
            }
        };
        let bw = self.p.text_width(&big, 44.0, DWRITE_FONT_WEIGHT_BOLD)?;
        self.p.text(&big, 44.0, DWRITE_FONT_WEIGHT_BOLD, rect(x, cy, bw + 2.0, 48.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let mut tx = x + bw + 2.0;
        if h.current.is_some() {
            self.p.text("%", 20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(tx, cy + 14.0, 24.0, 30.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
            tx += 20.0;
        }
        tx += 22.0;
        let lw = self.p.text_width(&line1.0, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
        self.p.text(&line1.0, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(tx, cy + 6.0, lw + 1.0, 18.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.p.text(&line1.1, 13.0, DWRITE_FONT_WEIGHT_NORMAL, rect(tx + lw, cy + 6.0, W - MARGIN - PAD - tx - lw, 18.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        self.p.text(&line2, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(tx, cy + 26.0, W - MARGIN - PAD - tx, 18.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        cy += 48.0 + 14.0;

        if let Some(current) = h.current {
            if view.table {
                let (l, r) = (x, W - MARGIN - PAD);
                self.p.text(t("Hónap", "Month"), 12.5, DWRITE_FONT_WEIGHT_MEDIUM, rect(l + 8.0, cy, 200.0, 24.0), sub, DWRITE_TEXT_ALIGNMENT_LEADING)?;
                self.p.text(t("Becsült kapacitás", "Estimated capacity"), 12.5, DWRITE_FONT_WEIGHT_MEDIUM, rect(r - 208.0, cy, 200.0, 24.0), sub, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                self.p.line(l, cy + 27.5, r, cy + 27.5, self.theme.border())?;
                let mut ry = cy + 28.0;
                for &(yr, m, v) in &h.months {
                    self.p.text(&year_month(yr, m), 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(l + 8.0, ry, 200.0, 24.0), fg, DWRITE_TEXT_ALIGNMENT_LEADING)?;
                    let v = v.map_or("–".to_string(), |v| format!("{}%", num(v, 1)));
                    self.p.text(&v, 12.5, DWRITE_FONT_WEIGHT_NORMAL, rect(r - 208.0, ry, 200.0, 24.0), fg, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                    ry += 24.0;
                }
            } else {
                self.health_chart(cy, &h, current)?;
            }
            cy += body;
        }
        self.p.paragraph(note, 11.5, rect(x, cy, inner, note_h), sub)?;
        y += card_h;
        Ok(y + 32.0)
    }

    /// A becsült kapacitás havonta, és 6 hónapos előrejelzés a mostani ütemmel.
    unsafe fn health_chart(&self, top: f32, h: &stats::Health, current: f32) -> Result<()> {
        let accent = self.accent;
        let (l, r) = (MARGIN + PAD + AXIS, W - MARGIN - PAD);
        let (ct, cb) = (top + 12.0, top + 160.0);
        let n = h.months.len();
        let proj = 6;
        let total = (n - 1 + proj).max(1) as f32;
        let min = h.months.iter().filter_map(|m| m.2).fold(current, f32::min);
        let lo = (((min - 5.0) / 5.0).floor() * 5.0).clamp(50.0, 75.0);
        let hx = |i: f32| l + (r - l) * i / total;
        let hy = |v: f32| cb - (cb - ct) * (v - lo) / (100.0 - lo);
        let mut v = 100.0;
        while v > lo + 0.1 {
            if (v - 80.0f32).abs() > 0.1 {
                self.grid_line(hy(v), &format!("{v}%"))?;
            }
            v -= 10.0;
        }
        let amber = self.theme.amber();
        self.p.polyline(&[(l, hy(80.0)), (r, hy(80.0))], 1.0, fade(amber, 0.7), &[4.0, 4.0])?;
        let label = t("80% – csere javasolt", "80% – consider replacing");
        let label_w = self.p.text_width(label, 11.0, DWRITE_FONT_WEIGHT_NORMAL)?;
        let label_r = rect(r - label_w - 6.0, hy(80.0) - 21.0, label_w + 6.0, 18.0);
        let pts: Vec<(f32, f32)> = h.months.iter().enumerate().filter_map(|(i, m)| m.2.map(|v| (hx(i as f32), hy(v)))).collect();
        let last = h.months.iter().rposition(|m| m.2.is_some()).unwrap_or(n - 1) as f32;
        let now_pt = (hx(last), hy(current));
        if let Some(p) = h.per_month {
            let end = (current - p * (total - last)).max(lo);
            self.p.polyline(&[now_pt, (hx(total), hy(end))], 2.0, fade(accent, 0.6), &[1.5, 2.5])?;
        }
        // A felirat a vonalak fölött, kártyaszínű alappal (az előrejelzés ne fusson át rajta).
        self.p.fill(label_r, 3.0, self.theme.row())?;
        self.p.text(label, 11.0, DWRITE_FONT_WEIGHT_NORMAL, label_r, amber, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        self.p.polyline(&pts, 2.0, accent, &[])?;
        self.p.circle(now_pt.0, now_pt.1, 6.5, self.theme.row())?;
        self.p.circle(now_pt.0, now_pt.1, 5.0, accent)?;
        self.p.text(t("ma", "now"), 11.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(now_pt.0 - 30.0, now_pt.1 - 26.0, 60.0, 16.0), self.theme.fg(1.0), DWRITE_TEXT_ALIGNMENT_CENTER)?;
        let (fy, fm) = (h.months[0].0, h.months[0].1);
        let step = ((n + proj) as f32 / 8.0).ceil().max(1.0) as usize;
        for i in (0..n + proj).step_by(step) {
            let m0 = fm as i32 - 1 + i as i32;
            let (yr, m) = (fy + m0.div_euclid(12), m0.rem_euclid(12) as u32 + 1);
            let label = if m == 1 || i == 0 { year_month(yr, m) } else { month_name(m).to_string() };
            self.tick(hx(i as f32), top + 182.0, &label)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tesztek a magyar alapnyelvvel futnak (a nyelv globális, ezért nem állítjuk át).
    #[test]
    fn formats() {
        assert_eq!(num(1.26, 1), "1,3");
        assert_eq!(split_duration(1.5), ("1".to_string(), "ó 30 p".to_string()));
        assert_eq!(split_duration(0.5), ("30".to_string(), "p".to_string()));
        assert_eq!(full_date(20_767), "2026. nov. 10.");
        assert_eq!(year_month(2027, 9), "2027. szept.");
    }
}
