//! Eszközszál: providerek futtatása, eszközváltozások figyelése, csatorna a UI felé.
//!
//! A szál egy eseményre alszik, amit a Windows HID eszközértesítése jelez
//! (CM_Register_Notification). Ébredés után rövid ideig gyűjti a további értesítéseket
//! (egy eszköz több interfésszel jelenik meg), majd a providerek újraolvassák az állapotot,
//! és csak a ténylegesen változott eszközök mennek a UI szálnak. Időzített olvasást csak az a
//! provider kér, amelynek eszköze magától nem jelez, és csak amíg van csatlakoztatott eszköze.
//!
//! Energiatakarékosság: kikapcsolt vagy zárolt képernyőnél az időzített olvasás szünetel
//! (úgysem látja senki), visszakapcsoláskor egy teljes frissítés jön; akkumulátoros üzemben
//! (laptop) az időközök duplázódnak. Ezeket a UI szál jelzi a `set_power` hívással.

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid;
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::model::DeviceState;
use crate::updater;
use crate::providers::bt_pnp::BtPnp;
use crate::providers::compx::Compx;
use crate::providers::logitech_hidpp::LogitechHidpp;
use crate::providers::{playstation::PlayStation, wired_detect::WiredDetect, xinput::XInput, Provider};

/// A UI ablaknak küldött üzenet: új állapotok várnak a csatornán.
pub const WM_DEVICES: u32 = WM_APP + 2;

/// Ennyi ideig gyűjtjük az egymás utáni eszközértesítéseket egy újraolvasás előtt.
const SETTLE_MS: u32 = 300;

/// A UI ablaknak küldött üzenet: új verzió érhető el (updater::AVAILABLE).
pub const WM_UPDATE: u32 = WM_APP + 3;

/// Első frissítés-ellenőrzés indulás után, majd ennyi időnként.
const UPDATE_FIRST: Duration = Duration::from_secs(60);
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Képernyő kikapcsolva vagy zárolva: nincs időzített olvasás.
static PAUSED: AtomicBool = AtomicBool::new(false);
/// Frissítések keresése (a beállításokból; "off" esetén false).
static UPDATES: AtomicBool = AtomicBool::new(true);

pub fn set_updates_enabled(on: bool) {
    UPDATES.store(on, Ordering::Relaxed);
}
/// Akkumulátoros üzem: dupla időközök.
static ON_BATTERY: AtomicBool = AtomicBool::new(false);
/// Az eszközszál ébresztő eseménye (HANDLE), hogy a változás azonnal érvényesüljön.
static WAKE: AtomicIsize = AtomicIsize::new(0);

/// Hibakereső napló a %TEMP%\napcat.log fájlba, csak ha a NAPCAT_LOG környezeti változó be van állítva.
pub fn log(msg: &str) {
    use std::io::Write;
    use std::sync::OnceLock;
    use std::time::SystemTime;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("NAPCAT_LOG").is_some()) {
        return;
    }
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let path = std::env::temp_dir().join("napcat.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{secs} {msg}");
    }
}

/// Energiaállapot a UI száltól; csak változáskor ébreszti az eszközszálat.
pub fn set_power(paused: bool, on_battery: bool) {
    let pause_changed = PAUSED.swap(paused, Ordering::Relaxed) != paused;
    let battery_changed = ON_BATTERY.swap(on_battery, Ordering::Relaxed) != on_battery;
    let changed = pause_changed || battery_changed;
    if changed {
        log(&format!("energia: szünet={paused} akkumulátor={on_battery}"));
    }
    let wake = WAKE.load(Ordering::Relaxed);
    if changed && wake != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(wake as *mut c_void));
        }
    }
}

pub fn spawn(hwnd: HWND) -> Receiver<DeviceState> {
    let (tx, rx) = channel();
    let hwnd = hwnd.0 as isize; // a HWND nem Send
    thread::Builder::new()
        .name("devices".into())
        .spawn(move || unsafe { run(HWND(hwnd as *mut c_void), tx) })
        .expect("eszközszál indítása");
    rx
}

unsafe fn run(hwnd: HWND, tx: Sender<DeviceState>) {
    let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    let Ok(changed) = CreateEventW(None, false, false, PCWSTR::null()) else { return };
    WAKE.store(changed.0 as isize, Ordering::Relaxed);
    let _notify = register_hid_notification(changed);

    let mut providers: Vec<Box<dyn Provider>> = vec![
        Box::new(WiredDetect::default()),
        Box::new(PlayStation::default()),
        Box::new(XInput::default()),
        Box::new(LogitechHidpp::default()),
        Box::new(BtPnp::default()),
        Box::new(Compx::default()),
    ];
    // Mikor esedékes a következő időzített olvasás providerenként (None: csak eseményre).
    let mut due: Vec<Option<Instant>> = vec![None; providers.len()];
    let mut last: HashMap<String, DeviceState> = HashMap::new();
    let mut device_change = true;
    let mut first = true;
    let mut update_due = Instant::now() + UPDATE_FIRST;
    loop {
        let now = Instant::now();
        // Frissítés-ellenőrzés ugyanazon az időzítőn; ritka (naponta), és csak egy kis HTTP kérés.
        if update_due <= now && !PAUSED.load(Ordering::Relaxed) {
            update_due = now + UPDATE_EVERY;
            if UPDATES.load(Ordering::Relaxed) {
                let found = updater::check_in_child(env!("CARGO_PKG_VERSION"));
                log(&format!("frissítés-ellenőrzés: {:?}", found.as_ref().map(|r| &r.version)));
                if found.is_some() {
                    let _ = PostMessageW(hwnd, WM_UPDATE, WPARAM(0), LPARAM(0));
                }
            }
        }
        let mut any = false;
        for (p, due) in providers.iter_mut().zip(due.iter_mut()) {
            if !device_change && !due.is_some_and(|d| d <= now) {
                continue;
            }
            let states = p.scan();
            log(&format!("olvasás ({}): {} eszköz", if device_change { "esemény" } else { "időzített" }, states.len()));
            for state in states {
                if last.get(&state.id) != Some(&state) {
                    last.insert(state.id.clone(), state.clone());
                    if tx.send(state).is_err() {
                        return; // a UI kilépett
                    }
                    any = true;
                }
            }
            let factor = if ON_BATTERY.load(Ordering::Relaxed) { 2 } else { 1 };
            *due = p.poll_interval().map(|i| now + i * factor);
        }
        // Az első olvasás után mindig jelzünk (akkor is, ha nincs eszköz), hogy a UI tudja: kész.
        if any || first {
            let _ = PostMessageW(hwnd, WM_DEVICES, WPARAM(0), LPARAM(0));
        }
        first = false;

        // Egyetlen időzítő az egész apphoz: alvás a legközelebbi esedékes olvasásig vagy a
        // következő eszközváltozásig (szünetben csak az utóbbira); utána a további értesítések
        // kivárása. Minden ébresztés (eszközváltozás, energiaállapot) teljes frissítés.
        let timeout = if PAUSED.load(Ordering::Relaxed) {
            INFINITE
        } else {
            let next = due.iter().flatten().copied().chain(Some(update_due)).min().unwrap_or(update_due);
            next.saturating_duration_since(Instant::now()).as_millis().min(u32::MAX as u128 - 1) as u32
        };
        device_change = WaitForSingleObject(changed, timeout) == WAIT_OBJECT_0;
        if device_change {
            while WaitForSingleObject(changed, SETTLE_MS) == WAIT_OBJECT_0 {}
        }
    }
}

/// HID interfész megjelenés/eltűnés értesítés; a visszahívás csak jelzi az eseményt.
/// A visszaadott regisztráció a szál végéig él.
unsafe fn register_hid_notification(event: HANDLE) -> Option<HCMNOTIFICATION> {
    let mut filter = CM_NOTIFY_FILTER {
        cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
        FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        ..Default::default()
    };
    filter.u.DeviceInterface.ClassGuid = HidD_GetHidGuid();
    let mut handle = HCMNOTIFICATION::default();
    let cr = CM_Register_Notification(&filter, Some(event.0 as *const c_void), Some(on_device_change), &mut handle);
    (cr == CR_SUCCESS).then_some(handle)
}

unsafe extern "system" fn on_device_change(
    _notify: HCMNOTIFICATION,
    context: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    let _ = SetEvent(HANDLE(context as *mut c_void));
    ERROR_SUCCESS.0
}
