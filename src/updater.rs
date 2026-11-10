//! Frissítés a GitHub Releases-ből (catninth/napcat), a közös catninth-updater könyvtárral.
//!
//! - Ellenőrzés és a telepítés menete (állapotgép, SemVer összevetés, csak stabil kiadás): a
//!   könyvtár `Updater`-e és `GitHubSource`-a.
//! - Telepítő: a NapCat saját `InstallBackend`-je. A NapCat-Setup-x.y.z.exe asset a kiválasztott
//!   tag alól jön (Windows WinHTTP), SHA-256 ellenőrzéssel a mellé feltöltött .sha256 fájl
//!   alapján (Windows BCrypt), majd %TEMP%-be kerül. Csak a saját repó release asset URL-je jöhet.
//! - Telepítés: a telepítő csendes módban indul, az app kilép, a telepítő a végén újraindítja.
//!
//! A hálózati munka (TLS, Tokio futtatókörnyezet) egy rövid életű segédfolyamatban (ugyanez az
//! exe) fut, ami utána kilép; a tálcán futó NapCat-ben nincs se Tokio, se TLS.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use catninth_updater::github::GitHubSource;
use catninth_updater::{DownloadCallback, Error, InstallBackend, Phase, PreparedUpdate, Repository, Updater};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};

pub const REPO: &str = "catninth/napcat";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub tag: String,
}

impl Release {
    fn setup_name(&self) -> String {
        format!("NapCat-Setup-{}.exe", self.version)
    }
}

/// A legutóbb talált, a futónál újabb kiadás (a UI menüje innen olvassa).
pub static AVAILABLE: Mutex<Option<Release>> = Mutex::new(None);

/// "v0.3.1" vagy "0.3.1" -> (0, 3, 1); prerelease ("-beta") és hibás formátum: None.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split('+').next()?; // a build metaadat nem számít
    if core.contains('-') {
        return None;
    }
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

pub fn is_newer(tag: &str, current: &str) -> bool {
    matches!((parse_version(tag), parse_version(current)), (Some(a), Some(b)) if a > b)
}

// --- catninth-updater (csak a segédfolyamatban fut) ---

/// A kiadás egy assetjének URL-je a kiválasztott tag alatt; csak a saját repóból.
fn asset_url(tag: &str, asset: &str) -> Option<String> {
    let url = Repository::parse(REPO).ok()?.asset_url(tag, asset).to_string();
    url.starts_with(&format!("https://github.com/{REPO}/releases/download/")).then_some(url)
}

/// `installer`: csak a letöltéshez kell.
fn build_updater(installer: Option<Arc<dyn InstallBackend>>) -> catninth_updater::Result<Updater> {
    let config = catninth_updater::Config::new(Repository::parse(REPO)?, env!("CARGO_PKG_VERSION"), 24 * 60)?
        .with_request_timeout(Duration::from_secs(60))?;
    let source = GitHubSource::new(config.repository().clone(), config.request_timeout())?;
    let mut builder = Updater::builder(config).source(Arc::new(source));
    if let Some(installer) = installer {
        builder = builder.installer(installer);
    }
    builder.build()
}

fn block_on<F: std::future::Future>(f: F) -> Option<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    Some(rt.block_on(f))
}

/// Some(kiadás), ha a GitHubon a futónál újabb stabil kiadás van.
fn check_latest() -> Option<Release> {
    block_on(async {
        let updater = build_updater(None).ok()?;
        if let Err(e) = updater.fetch_version().await {
            eprintln!("frissítés-ellenőrzés: {e}");
            return None;
        }
        let state = updater.state();
        let latest = state.latest.filter(|_| state.phase == Phase::Available)?;
        Some(Release { version: latest.version.to_string(), tag: latest.tag })
    })?
}

/// A NapCat telepítője: letöltés a kiválasztott tag alól, SHA-256 ellenőrzés, mentés %TEMP%-be.
/// A futtatás a fő folyamat dolga (utána kilép), ezért az `install` csak elmenti a fájlt.
struct SetupInstaller {
    /// Csak ez a verzió telepíthető (amit a felhasználó elfogadott), akkor is, ha közben újabb jött.
    version: String,
    saved: Arc<Mutex<Option<PathBuf>>>,
}

struct PreparedSetup {
    release: Release,
    setup_url: String,
    sha256_url: String,
    saved: Arc<Mutex<Option<PathBuf>>>,
}

fn installer_error(m: &str) -> Error {
    Error::Installer(m.into())
}

#[async_trait]
impl InstallBackend for SetupInstaller {
    async fn prepare(&self, release: &catninth_updater::Release) -> catninth_updater::Result<Box<dyn PreparedUpdate>> {
        let release = Release { version: release.version.to_string(), tag: release.tag.clone() };
        if release.version != self.version {
            return Err(installer_error("közben másik kiadás lett a legfrissebb"));
        }
        let name = release.setup_name();
        let setup_url = asset_url(&release.tag, &name).ok_or_else(|| installer_error("hibás asset URL"))?;
        let sha256_url = asset_url(&release.tag, &format!("{name}.sha256")).ok_or_else(|| installer_error("hibás asset URL"))?;
        Ok(Box::new(PreparedSetup { release, setup_url, sha256_url, saved: self.saved.clone() }))
    }
}

#[async_trait]
impl PreparedUpdate for PreparedSetup {
    /// A bájtok csak SHA-256 egyezés után jönnek vissza.
    async fn download(&self, progress: DownloadCallback) -> catninth_updater::Result<Vec<u8>> {
        let (setup, sha) = (self.setup_url.clone(), self.sha256_url.clone());
        tokio::task::spawn_blocking(move || download_verified(&setup, &sha, |received, total| progress(received, total)))
            .await
            .map_err(|e| Error::Task(e.to_string()))?
            .ok_or_else(|| installer_error("a letöltés vagy az SHA-256 ellenőrzés nem sikerült"))
    }

    async fn install(&self, bytes: Vec<u8>) -> catninth_updater::Result<()> {
        let path = std::env::temp_dir().join(self.release.setup_name());
        std::fs::write(&path, bytes)?;
        *self.saved.lock().map_err(|_| Error::Task("mentési zár".into()))? = Some(path);
        Ok(())
    }
}

/// Az ellenőrzött telepítő útvonala, ha a legfrissebb kiadás `version`.
fn download_latest(version: &str) -> Option<PathBuf> {
    let saved = Arc::new(Mutex::new(None));
    let backend = Arc::new(SetupInstaller { version: version.to_string(), saved: saved.clone() });
    block_on(async {
        let updater = build_updater(Some(backend)).ok()?;
        updater.fetch_version().await.ok()?;
        match updater.install().await {
            Ok(()) => Some(()),
            Err(e) => {
                eprintln!("frissítés letöltés: {e}");
                None
            }
        }
    })??;
    let path = saved.lock().ok()?.clone()?;
    path.is_file().then_some(path)
}

// --- segédfolyamat ---

/// A segédfolyamat belépési pontja: Some(kilépési kód), ha az argumentumok updater parancsot kérnek.
/// Kimenet: ellenőrzésnél "release<TAB><verzió><TAB><tag>" (vagy semmi), letöltésnél a telepítő útvonala.
pub fn run_cli(args: &[String]) -> Option<i32> {
    use std::io::Write;
    let mut out = String::new();
    let code = match args.get(1).map(String::as_str) {
        Some("--update-check") => {
            if let Some(r) = check_latest() {
                out.push_str(&format!("release\t{}\t{}\n", r.version, r.tag));
            }
            0
        }
        Some("--update-download") => match args.get(2).and_then(|v| download_latest(v)) {
            Some(path) => {
                out.push_str(&path.to_string_lossy());
                0
            }
            None => 1,
        },
        _ => return None,
    };
    let _ = std::io::stdout().write_all(out.as_bytes());
    Some(code)
}

fn run_child(args: &[&str]) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let output = std::process::Command::new(exe).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Ellenőrzés segédfolyamatban; az elérhető kiadást ebben a folyamatban tárolja.
pub fn check_in_child(current: &str) -> Option<Release> {
    let out = run_child(&["--update-check"])?;
    let release = out.lines().find_map(|line| match line.split('\t').collect::<Vec<_>>().as_slice() {
        ["release", v, tag] => Some(Release { version: v.to_string(), tag: tag.to_string() }),
        _ => None,
    });
    let release = release.filter(|r| is_newer(&r.version, current));
    *AVAILABLE.lock().ok()? = release.clone();
    release
}

/// Letöltés és ellenőrzés segédfolyamatban; az ellenőrzött telepítő útvonala.
pub fn download_in_child(release: &Release) -> Option<PathBuf> {
    let out = run_child(&["--update-download", &release.version])?;
    let path = PathBuf::from(out.trim());
    path.is_file().then_some(path)
}

/// A telepítő indítása csendes módban; utána az app kilép, a telepítő újraindítja.
pub fn run_installer(path: &PathBuf) -> bool {
    std::process::Command::new(path).args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).spawn().is_ok()
}

// --- HTTP és SHA-256 (Windows beépített WinHTTP és BCrypt) ---

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    unsafe {
        let _ = BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out);
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A telepítő bájtjai, ha a hash egyezik a .sha256 fájlban ("<hash>  <fájlnév>" vagy csak a hash).
fn download_verified(setup_url: &str, sha256_url: &str, progress: impl Fn(u64, Option<u64>)) -> Option<Vec<u8>> {
    let mut sum_text = Vec::new();
    let r = get(sha256_url, "", |c| {
        sum_text.extend_from_slice(c);
        sum_text.len() < 4096
    })?;
    if r.status != 200 {
        return None;
    }
    let expected = String::from_utf8_lossy(&sum_text).split_whitespace().next()?.to_ascii_lowercase();

    let mut data = Vec::new();
    let r = get(setup_url, "", |c| {
        data.extend_from_slice(c);
        progress(data.len() as u64, None);
        data.len() < 64 * 1024 * 1024
    })?;
    (r.status == 200 && hex(&sha256(&data)) == expected).then_some(data)
}

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

struct Response {
    status: u32,
}

/// HTTPS GET a WinHTTP-vel; a törzs a `sink`-be megy darabonként. Az átirányításokat (pl. a
/// release assetek CDN-jére) a WinHTTP követi.
fn get(url: &str, headers: &str, mut sink: impl FnMut(&[u8]) -> bool) -> Option<Response> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    unsafe {
        let session = Handle(WinHttpOpen(w!("NapCat-Updater"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return None;
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000).ok()?;
        let host_w = wide(host);
        let connect = Handle(WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT as u16, 0));
        if connect.0.is_null() {
            return None;
        }
        let path_w = wide(&path);
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            w!("GET"),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return None;
        }
        let headers_w: Vec<u16> = headers.encode_utf16().collect();
        if !headers_w.is_empty() {
            WinHttpAddRequestHeaders(request.0, &headers_w, WINHTTP_ADDREQ_FLAG_ADD).ok()?;
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).ok()?;

        let mut status = 0u32;
        let mut len = 4u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .ok()?;

        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read).ok()?;
            if read == 0 {
                break;
            }
            if !sink(&buf[..read as usize]) {
                return None;
            }
        }
        Some(Response { status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.3.1"), Some((0, 3, 1)));
        assert_eq!(parse_version("1.2.3+build.5"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3-beta"), None);
        assert_eq!(parse_version("1.2"), None);
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v0.10.0", "0.9.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[test]
    fn asset_urls() {
        let r = Release { version: "0.2.0".into(), tag: "v0.2.0".into() };
        assert_eq!(asset_url(&r.tag, &r.setup_name()).as_deref(), Some("https://github.com/catninth/napcat/releases/download/v0.2.0/NapCat-Setup-0.2.0.exe"));
        // A tag nem szökhet ki a repó release útvonalából.
        assert!(!asset_url("../../evil/x", "a.exe").unwrap_or_default().contains("/evil/"));
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// Valódi hálózati kérés (csak olvasás): `cargo test live_check -- --ignored --nocapture`.
    #[test]
    #[ignore = "hálózatot használ"]
    fn live_check() {
        let latest = block_on(async {
            let updater = build_updater(None).unwrap();
            updater.fetch_version().await.map(|r| r.map(|r| r.tag))
        });
        println!("legfrissebb kiadás: {latest:?}, újabb a futónál: {:?}", check_latest());
        // Release asset letöltése: a github.com -> CDN átirányítást a WinHTTP-nek követnie kell.
        let r = Release { version: "0.1.1".into(), tag: "v0.1.1".into() };
        let name = r.setup_name();
        let data = download_verified(&asset_url(&r.tag, &name).unwrap(), &asset_url(&r.tag, &format!("{name}.sha256")).unwrap(), |_, _| {});
        println!("v0.1.1 telepítő ellenőrizve: {:?} bájt", data.map(|d| d.len()));
    }
}
