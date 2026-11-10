//! Képernyőképek a README-hez és a docs/gallery oldalhoz, szimulált eszközökkel.
//!
//! A panel, a tooltip és a beállítások ablak ugyanazzal a Direct2D kóddal rajzolódik, mint az
//! alkalmazásban, csak ablak helyett egy 2x felbontású (192 DPI) bitképre. A tálcaikonok a valódi
//! 32 px-es (200%-os) tálcaképek. A toastok valódi Windows értesítések, a képernyőről kivágva.
//!
//! `cargo test --release gallery -- --ignored --nocapture --test-threads=1`

use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::{Interface, Result, PCWSTR};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};

use super::draw::{rect, Painter};
use super::icon_render::{AppIcon, IconRenderer, Layout};
use super::theme::{self, rgb, Theme};
use super::{flyout, settings_window, tooltip};
use crate::model::{ChargeState, DeviceKind, DeviceState};
use crate::providers::custom::Candidate;
use crate::settings::Settings;

const DPI: f32 = 192.0;

fn dev(id: &str, name: &str, kind: DeviceKind, level: Option<u8>, charge: ChargeState, connected: bool, estimate: Option<&str>) -> DeviceState {
    DeviceState {
        id: id.into(),
        name: name.into(),
        kind,
        level,
        level_approx: false,
        charge,
        connected,
        hw: None,
        estimate: estimate.map(Into::into),
    }
}

/// A szimulált eszközök: egér, töltő egér, lemerülő billentyűzet, kontroller, ismeretlen szintű
/// headset és egy leválasztott kontroller.
fn devices() -> Vec<DeviceState> {
    vec![
        dev("vgn", "VGN Dragonfly F1", DeviceKind::Mouse, Some(50), ChargeState::Discharging, true, Some("about 2 days")),
        dev("mchose", "MCHOSE A7 V2 Ultra+", DeviceKind::Mouse, Some(64), ChargeState::Charging, true, Some("1 h 10 min to full")),
        dev("rk", "RK R75 Pro", DeviceKind::Keyboard, Some(9), ChargeState::Discharging, true, Some("about 40 min")),
        dev("ds4", "DualShock 4", DeviceKind::Controller, Some(72), ChargeState::Discharging, true, Some("about 5 hours")),
        dev("g435", "Logitech G435", DeviceKind::Headset, None, ChargeState::Unknown, true, None),
        dev("xbox", "Xbox controller", DeviceKind::Controller, None, ChargeState::Unknown, false, Some("2 hours ago")),
    ]
}

fn out_dir(area: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs").join("gallery").join(area);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Canvas {
    wic: IWICImagingFactory,
    renderer: IconRenderer,
}

impl Canvas {
    fn new() -> Canvas {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            Canvas {
                wic: CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap(),
                renderer: IconRenderer::new().unwrap(),
            }
        }
    }

    /// `w` x `h` DIP-es rajz `dpi` felbontásban egy új bitképre.
    unsafe fn render(&self, w: f32, h: f32, dpi: f32, f: impl FnOnce(&ID2D1DeviceContext) -> Result<()>) -> IWICBitmap {
        let k = dpi / 96.0;
        let (pw, ph) = ((w * k).round() as u32, (h * k).round() as u32);
        let bmp = self.wic.CreateBitmap(pw, ph, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand).unwrap();
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: dpi,
            dpiY: dpi,
            ..Default::default()
        };
        let ctx: ID2D1DeviceContext = self.renderer.d2d().CreateWicBitmapRenderTarget(&bmp, &props).unwrap().cast().unwrap();
        ctx.BeginDraw();
        f(&ctx).unwrap();
        ctx.EndDraw(None, None).unwrap();
        bmp
    }

    unsafe fn save(&self, bmp: &IWICBitmap, path: &PathBuf) {
        let wide: Vec<u16> = path.display().to_string().encode_utf16().chain(Some(0)).collect();
        let stream = self.wic.CreateStream().unwrap();
        stream.InitializeFromFilename(PCWSTR(wide.as_ptr()), GENERIC_WRITE.0).unwrap();
        let enc = self.wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).unwrap();
        enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let (mut frame, mut bag) = (None, None);
        enc.CreateNewFrame(&mut frame, &mut bag).unwrap();
        let frame = frame.unwrap();
        frame.Initialize(bag.as_ref()).unwrap();
        let (mut w, mut h) = (0, 0);
        bmp.GetSize(&mut w, &mut h).unwrap();
        frame.SetSize(w, h).unwrap();
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        bmp.GetPixelFormat().map(|f| fmt = f).unwrap();
        frame.SetPixelFormat(&mut fmt).unwrap();
        frame.WriteSource(bmp, std::ptr::null()).unwrap();
        frame.Commit().unwrap();
        enc.Commit().unwrap();
        println!("{} ({w}x{h})", path.display());
    }
}

/// Az ablakok témája és kiemelő színe: a design kékje (sötét: 4CC2FF, világos: 005FB8), hogy a
/// képek ne a gép Windows kiemelő színétől függjenek.
fn use_theme(light: bool) -> Theme {
    theme::configure(if light { "light" } else { "dark" }, true, Some(if light { 0x005FB8 } else { 0x4CC2FF }), false);
    Theme { light }
}

#[test]
#[ignore = "felülírja a docs/gallery képeit"]
fn gallery() {
    // A galéria angol felülettel készül.
    crate::i18n::set("en");
    let c = Canvas::new();
    let devices = devices();
    let candidates = vec![
        Candidate { vid: 0x3142, pid: 0x0A80, product: "fifine Microphone".into(), kind: DeviceKind::Headset },
        Candidate { vid: 0x258A, pid: 0x0049, product: "Gaming Keyboard".into(), kind: DeviceKind::Keyboard },
    ];
    // A Névjegy szakasz elérhető frissítéssel (Telepítés gomb).
    *crate::updater::AVAILABLE.lock().unwrap() = Some(crate::updater::Release { version: "0.2.0".into(), tag: "v0.2.0".into() });
    for light in [false, true] {
        let t = use_theme(light);
        let name = if light { "light" } else { "dark" };
        unsafe {
            for grid in [false, true] {
                let (w, h) = flyout::gallery_size(&devices, grid);
                let bmp = c.render(w, h, DPI, |ctx| flyout::gallery_draw(ctx, &c.renderer, &devices, grid, Some(5), t));
                let file = format!("{}-{name}.png", if grid { "grid" } else { "list" });
                c.save(&bmp, &out_dir("flyout").join(file));
            }

            let (w, h) = tooltip::size(devices.len());
            let bmp = c.render(w, h, DPI, |ctx| tooltip::draw(ctx, &c.renderer, &devices, t));
            c.save(&bmp, &out_dir("tooltip").join(format!("{name}.png")));

            let s = Settings::default();
            for (page, file) in [(settings_window::Page::Settings, "general"), (settings_window::Page::Devices, "devices")] {
                // Először a magasság (egy kis bitképre rajzolva), aztán a teljes oldal.
                let mut height = 0.0;
                c.render(1.0, 1.0, 96.0, |ctx| {
                    height = settings_window::gallery_draw(ctx, &c.renderer, page, &s, &devices, &candidates, t)?;
                    Ok(())
                });
                let bmp = c.render(settings_window::GALLERY_W, height, DPI, |ctx| {
                    settings_window::gallery_draw(ctx, &c.renderer, page, &s, &devices, &candidates, t).map(|_| ())
                });
                c.save(&bmp, &out_dir("settings").join(format!("{file}-{name}.png")));
            }
        }
    }
    unsafe {
        tray_styles(&c);
        app_icons(&c);
    }
}

/// Szimulált előzmények a Statisztika oldalhoz: az egér öt hónapja (egyre gyorsabban merül, ahogy
/// kopik az akku), a billentyűzet és a kontroller néhány hete. Minden nap 9 és 23 óra között megy
/// a gép; 15% alatt az eszköz töltőre kerül. Determinisztikus (saját álvéletlen).
fn stats_log(now: i64) -> std::collections::HashMap<String, Vec<crate::stats::Sample>> {
    use crate::stats::{Sample, NONE};
    let offset = 3600;
    let today = (now + offset).div_euclid(86400);
    let mut seed = 12345u32;
    let mut rnd = move || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (seed >> 16) as f32 / 65536.0
    };
    let mut log = std::collections::HashMap::new();
    for (id, days, rate, wear, charge, start) in
        [("vgn", 150, 1.1, 0.075, 40.0, 78.0), ("rk", 40, 0.45, 0.0, 30.0, 62.0), ("ds4", 20, 6.5, 0.0, 45.0, 70.0)]
    {
        let mut v: Vec<Sample> = Vec::new();
        let mut level: f32 = start;
        let mut charging = false;
        for d in (today - days)..=today {
            let on = d * 86400 - offset + 9 * 3600;
            let off = (d * 86400 - offset + 23 * 3600).min(now);
            if on >= now {
                break;
            }
            // Napi használat: a hétvégén és néhány napon több, máskor kevesebb.
            let intensity = 0.6 + rnd() * 0.8;
            let r = rate * intensity / (1.0 - wear * (days - (today - d)) as f32 / days as f32);
            let mut last: Option<(u8, u8)> = None;
            let mut t = on;
            while t < off {
                if charging {
                    level += charge / 6.0;
                    if level >= 100.0 {
                        level = 100.0;
                        charging = false;
                    }
                } else {
                    level -= r / 6.0 * (0.7 + rnd() * 0.6);
                    if level < 15.0 && rnd() < 0.3 {
                        charging = true;
                    }
                    level = level.max(3.0);
                }
                let code = if charging { b'c' } else if level >= 100.0 { b'f' } else { b'd' };
                let entry = (code, level as u8);
                if last != Some(entry) {
                    v.push(Sample { t, level: entry.1, code });
                    last = Some(entry);
                }
                t += 600;
            }
            if off < now {
                v.push(Sample { t: off, level: NONE, code: b'B' });
            }
        }
        log.insert(id.to_string(), v);
    }
    log
}

/// A Statisztika oldal (sötét: 7 nap, rámutatással a töltöttség grafikonon; világos: 30 nap).
#[test]
#[ignore = "felülírja a docs/gallery képeit"]
fn gallery_stats() {
    crate::i18n::set("en");
    let c = Canvas::new();
    let devices = devices();
    // 2026. nov. 10., kedd, 18:30 helyi idő (UTC+1).
    let now = 20_767 * 86400 + 17 * 3600 + 1800;
    for light in [false, true] {
        let t = use_theme(light);
        let view = settings_window::GalleryStatsView { dev: None, range: if light { 30 } else { 7 }, table: false };
        // Péntek délután a töltöttség grafikonon.
        let hover = (!light).then_some((300.0, 330.0));
        unsafe {
            let mut height = 0.0;
            c.render(1.0, 1.0, 96.0, |ctx| {
                height = settings_window::gallery_draw_stats(ctx, &c.renderer, &devices, stats_log(now), now, view.clone(), hover, t)?;
                Ok(())
            });
            let bmp = c.render(settings_window::GALLERY_W, height, DPI, |ctx| {
                settings_window::gallery_draw_stats(ctx, &c.renderer, &devices, stats_log(now), now, view.clone(), hover, t).map(|_| ())
            });
            c.save(&bmp, &out_dir("stats").join(format!("{}.png", if light { "light" } else { "dark" })));
        }
    }
}

/// A tálcaikon stílusok: soronként egy stílus, oszloponként egy eszközállapot, sötét és világos
/// tálcán. A valódi 32 px-es (200%) ikonképek, kétszeresre nagyítva (pixelhűen).
unsafe fn tray_styles(c: &Canvas) {
    let states = [
        dev("a", "", DeviceKind::Mouse, Some(50), ChargeState::Discharging, true, None),
        dev("b", "", DeviceKind::Mouse, Some(64), ChargeState::Charging, true, None),
        dev("c", "", DeviceKind::Keyboard, Some(9), ChargeState::Discharging, true, None),
        dev("d", "", DeviceKind::Controller, Some(72), ChargeState::Discharging, true, None),
        dev("e", "", DeviceKind::Headset, None, ChargeState::Unknown, true, None),
        dev("f", "", DeviceKind::Keyboard, Some(100), ChargeState::Full, true, None),
        dev("g", "", DeviceKind::Controller, None, ChargeState::Unknown, false, None),
    ];
    let styles = [("Ring", Layout::Ring), ("Percent", Layout::Percent), ("Badge", Layout::Pie), ("Bar", Layout::Bar)];
    let (label_w, cell, head, pad) = (84.0f32, 44.0f32, 30.0f32, 16.0f32);
    let half = label_w + states.len() as f32 * cell + 2.0 * pad;
    let (w, h) = (2.0 * half, head + styles.len() as f32 * cell + pad);
    let mut r = IconRenderer::new().unwrap();
    // Az ikonok pixelei előre (a rajzoló tálca elrendezése stílusonként változik).
    let mut pixels = Vec::new();
    for (_, layout) in styles {
        r.tray_layout = layout;
        for light in [false, true] {
            for d in &states {
                pixels.push(r.render_pixels(d, 32, Theme { light }).unwrap());
            }
        }
    }
    let bmp = c.render(w, h, DPI, |ctx| {
        let p = Painter { ctx, renderer: &c.renderer };
        for (ti, light) in [false, true].into_iter().enumerate() {
            let t = Theme { light };
            let x0 = ti as f32 * half;
            p.fill(rect(x0, 0.0, half, h), 0.0, rgb(if light { 0xEEEEEE } else { 0x1C1C1C }, 1.0))?;
            let title = if light { "Light taskbar" } else { "Dark taskbar" };
            p.text(title, 11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, rect(x0 + pad, 6.0, half - 2.0 * pad, head - 8.0), t.sub(), DWRITE_TEXT_ALIGNMENT_LEADING)?;
            for (si, (label, _)) in styles.iter().enumerate() {
                let y = head + si as f32 * cell;
                p.text(label, 12.0, DWRITE_FONT_WEIGHT_NORMAL, rect(x0 + pad, y, label_w, cell), t.fg(1.0), DWRITE_TEXT_ALIGNMENT_LEADING)?;
                for di in 0..states.len() {
                    let px = &pixels[(si * 2 + ti) * states.len() + di];
                    let props = D2D1_BITMAP_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                        dpiX: DPI,
                        dpiY: DPI,
                    };
                    let rt: ID2D1RenderTarget = ctx.cast()?;
                    let icon = rt.CreateBitmap(D2D_SIZE_U { width: 32, height: 32 }, Some(px.as_ptr() as *const c_void), 32 * 4, &props)?;
                    // 32 px 192 DPI-n = 16 DIP; a cellában 32 DIP-re nagyítva.
                    let (x, iy) = (x0 + pad + label_w + di as f32 * cell + (cell - 32.0) / 2.0, y + (cell - 32.0) / 2.0);
                    rt.DrawBitmap(&icon, Some(&rect(x, iy, 32.0, 32.0)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
                }
            }
        }
        Ok(())
    });
    c.save(&bmp, &out_dir("tray").join("icon-styles.png"));
}

/// Az app ikon három állapota (sötét, világos, feltöltve) a saját témájú hátterén.
unsafe fn app_icons(c: &Canvas) {
    let items = [("Dark", AppIcon::Dark, 0x1C1D21u32, false), ("Light", AppIcon::Light, 0xF3F3F5, true), ("Charged", AppIcon::Charged, 0x1C1D21, false)];
    let (cell, icon, label) = (176.0f32, 112.0f32, 28.0f32);
    let bmp = c.render(cell * items.len() as f32, cell + label, DPI, |ctx| {
        let p = Painter { ctx, renderer: &c.renderer };
        for (i, (name, style, bg, light)) in items.iter().enumerate() {
            let x = i as f32 * cell;
            p.fill(rect(x, 0.0, cell, cell + label), 0.0, rgb(*bg, 1.0))?;
            ctx.SetTransform(&Matrix3x2::translation(x + (cell - icon) / 2.0, (cell - icon) / 2.0 + 8.0));
            c.renderer.draw_app_icon(ctx, icon, *style)?;
            ctx.SetTransform(&Matrix3x2::identity());
            let t = Theme { light: *light };
            p.text(name, 12.5, DWRITE_FONT_WEIGHT_MEDIUM, rect(x, cell - 4.0, cell, label), t.sub(), DWRITE_TEXT_ALIGNMENT_CENTER)?;
        }
        Ok(())
    });
    c.save(&bmp, &out_dir("tray").join("app-icon-states.png"));
}

/// Valódi toastok (lemerülés, teli töltés) a képernyőről kivágva; utána törli őket az
/// értesítési központból. A gépen be kell legyenek kapcsolva az értesítések.
#[test]
#[ignore = "toastot mutat a képernyőn, felülírja a docs/gallery képeit"]
fn gallery_toasts() {
    use windows::core::{w, HSTRING};
    use windows::UI::Notifications::ToastNotificationManager;
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::*;
    // A galéria angol felülettel készül.
    crate::i18n::set("en");
    let c = Canvas::new();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        super::toast::register();
        let hwnd = CreateWindowExW(Default::default(), w!("STATIC"), None, WINDOW_STYLE(0), 0, 0, 0, 0, HWND_MESSAGE, None, None, None).unwrap();
        let history = ToastNotificationManager::History().unwrap();
        let aumid = HSTRING::from("catninth.NapCat");
        let _ = history.ClearWithId(&aumid);
        std::thread::sleep(std::time::Duration::from_secs(2));
        let shots = [
            ("low-battery", "RK R75 Pro is running low", "RK R75 Pro: 9% left. Time to charge it.", Some("rk")),
            ("fully-charged", "MCHOSE A7 V2 Ultra+ is fully charged", "100%, you can unplug the charger.", None),
        ];
        for (file, title, text, snooze) in shots {
            super::toast::show(hwnd, title, text, snooze).unwrap();
            // A beúszó animáció vége.
            std::thread::sleep(std::time::Duration::from_millis(2500));
            match toast_rect() {
                Some(r) => {
                    let bmp = screen_bitmap(&c, r);
                    c.save(&bmp, &out_dir("notifications").join(format!("{file}.png")));
                }
                None => println!("{file}: nem található a toast ablaka"),
            }
            let _ = history.ClearWithId(&aumid);
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
}

/// A látszó toast helye. A Windows 11 a toastot egy shell felületen belül rajzolja (nincs saját
/// ablaka), ezért a munkaterület jobb alsó sarkában a sötét toast 1 px-es kerete (szürke ~47)
/// és akril kitöltése (~30–36) alapján keressük meg a széleit.
unsafe fn toast_rect() -> Option<windows::Win32::Foundation::RECT> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let mut work = RECT::default();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut RECT as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).ok()?;
    let (w, h) = (700, 500);
    let area = RECT { left: work.right - w, top: work.bottom - h, right: work.right, bottom: work.bottom };
    let px = screen_pixels(area);
    let gray = |x: i32, y: i32| -> Option<u8> {
        if x < 0 || y < 0 || x >= w || y >= h {
            return None;
        }
        let i = ((y * w + x) * 4) as usize;
        let (b, g, r) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
        ((r - g).abs() <= 3 && (g - b).abs() <= 3).then_some(g as u8)
    };
    let border = |x: i32, y: i32| gray(x, y).is_some_and(|g| (43..=52).contains(&g));
    let fill = |x: i32, y: i32| gray(x, y).is_some_and(|g| (26..=40).contains(&g));
    // Egy sor a toast alján (a munkaterület alja fölött 30 px), két irányból a keretig.
    let y = h - 30;
    let left = (0..w - 1).find(|&x| border(x, y) && fill(x + 1, y) && fill(x + 2, y))?;
    let right = (1..w).rev().find(|&x| border(x, y) && fill(x - 1, y) && fill(x - 2, y))?;
    // Egy oszlop a bal belső margóban (a lekerekítésen belül), két irányból a keretig.
    let x = left + 12;
    let top = (0..h - 1).find(|&y| border(x, y) && fill(x, y + 1) && fill(x, y + 2))?;
    let bottom = (1..h).rev().find(|&y| border(x, y) && fill(x, y - 1) && fill(x, y - 2))?;
    (right - left > 200 && bottom - top > 60).then_some(RECT {
        left: area.left + left,
        top: area.top + top,
        right: area.left + right + 1,
        bottom: area.top + bottom + 1,
    })
}

/// A képernyő egy része WIC bitképként.
unsafe fn screen_bitmap(c: &Canvas, r: windows::Win32::Foundation::RECT) -> IWICBitmap {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let px = screen_pixels(r);
    c.wic.CreateBitmapFromMemory(w as u32, h as u32, &GUID_WICPixelFormat32bppBGRA, (w * 4) as u32, &px).unwrap()
}

/// A képernyő egy része BGRA pixelekként (átlátszatlan).
unsafe fn screen_pixels(r: windows::Win32::Foundation::RECT) -> Vec<u8> {
    use windows::Win32::Graphics::Gdi::*;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(screen);
    let bitmap = CreateCompatibleBitmap(screen, w, h);
    let old = SelectObject(mem, bitmap);
    BitBlt(mem, 0, 0, w, h, screen, r.left, r.top, ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0)).unwrap();
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    GetDIBits(mem, bitmap, 0, h as u32, Some(px.as_mut_ptr() as *mut c_void), &mut info, DIB_RGB_COLORS);
    SelectObject(mem, old);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
    for p in px.chunks_exact_mut(4) {
        p[3] = 255;
    }
    px
}
