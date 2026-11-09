//! Frissítés a GitHub Releases-ből (catninth/napcat).
//!
//! - Ellenőrzés: GET /repos/<repo>/releases/latest, ETag / If-None-Match fejléccel, így változatlan
//!   esetben 304-es üres válasz jön. A /latest végpont a draft és prerelease kiadásokat kihagyja.
//! - HTTP: a Windows beépített WinHTTP-je (tanúsítványtár, proxy), nincs TLS könyvtár az exe-ben.
//! - Letöltés: a NapCat-Setup-x.y.z.exe asset %TEMP%-be, majd SHA-256 ellenőrzés a mellé feltöltött
//!   .sha256 fájl alapján (Windows BCrypt). Csak a saját repó release asset URL-je fogadható el.
//! - Telepítés: a telepítő csendes módban indul, az app kilép, a telepítő a végén újraindítja.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};

pub const REPO: &str = "catninth/napcat";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub setup_url: String,
    pub sha256_url: String,
}

/// A legutóbb talált, a futónál újabb kiadás (a UI menüje innen olvassa).
pub static AVAILABLE: Mutex<Option<Release>> = Mutex::new(None);
/// Az utolsó sikeres válasz ETag-je; változatlan kiadásnál a GitHub 304-et ad.
static ETAG: Mutex<Option<String>> = Mutex::new(None);

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

/// Egy JSON szövegmező összes értéke (pl. "tag_name"), az escape-ek feloldásával. A GitHub
/// válasza jól formált, és nekünk csak néhány egyszerű mező kell, ezért nem kell JSON könyvtár.
fn json_strings(json: &str, key: &str) -> Vec<String> {
    let pattern = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(pos) = rest.find(&pattern) {
        rest = &rest[pos + pattern.len()..];
        let after = rest.trim_start();
        let Some(after) = after.strip_prefix(':') else { continue };
        let Some(value) = after.trim_start().strip_prefix('"') else { continue };
        let mut s = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            s.push(ch);
                        }
                    }
                    Some(other) => s.push(other), // \" \\ \/
                    None => break,
                },
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

/// A releases/latest válaszából a kiadás, ha a szükséges assetek megvannak és a saját repóból jönnek.
pub fn parse_release(json: &str, repo: &str) -> Option<Release> {
    let tag = json_strings(json, "tag_name").into_iter().next()?;
    let version = tag.trim_start_matches('v').to_string();
    parse_version(&version)?;
    let prefix = format!("https://github.com/{repo}/releases/download/");
    let urls: Vec<String> = json_strings(json, "browser_download_url").into_iter().filter(|u| u.starts_with(&prefix)).collect();
    let setup_name = format!("NapCat-Setup-{version}.exe");
    let setup_url = urls.iter().find(|u| u.ends_with(&format!("/{setup_name}")))?.clone();
    let sha256_url = urls.iter().find(|u| u.ends_with(&format!("/{setup_name}.sha256")))?.clone();
    Some(Release { version, setup_url, sha256_url })
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
    etag: Option<String>,
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
        let mut etag_buf = [0u16; 256];
        let mut etag_len = (etag_buf.len() * 2) as u32;
        let etag = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_ETAG,
            PCWSTR::null(),
            Some(etag_buf.as_mut_ptr() as *mut c_void),
            &mut etag_len,
            std::ptr::null_mut(),
        )
        .ok()
        .map(|_| String::from_utf16_lossy(&etag_buf[..etag_len as usize / 2]));

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
        Some(Response { status, etag })
    }
}

enum Fetched {
    /// 304: a legutóbbi válasz óta nem változott.
    Unchanged,
    /// A legfrissebb kiadás (ha érvényes és a szükséges assetek megvannak) és az új ETag.
    Latest(Option<Release>, Option<String>),
}

fn fetch(repo: &str, etag: Option<&str>) -> Option<Fetched> {
    let mut headers = String::from("Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n");
    if let Some(etag) = etag {
        headers.push_str(&format!("If-None-Match: {etag}\r\n"));
    }
    let mut body = Vec::new();
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let response = get(&url, &headers, |chunk| {
        body.extend_from_slice(chunk);
        body.len() < 1024 * 1024
    })?;
    match response.status {
        304 => Some(Fetched::Unchanged),
        200 => Some(Fetched::Latest(parse_release(&String::from_utf8_lossy(&body), repo), response.etag)),
        _ => None, // 404: még nincs kiadás; 403: rate limit
    }
}

/// Ellenőrzés ebben a folyamatban: Some(kiadás), ha a GitHubon a futónál újabb stabil kiadás van.
#[cfg(test)]
pub fn check(repo: &str, current: &str) -> Option<Release> {
    let etag = ETAG.lock().ok()?.clone();
    match fetch(repo, etag.as_deref())? {
        Fetched::Unchanged => AVAILABLE.lock().ok()?.clone(),
        Fetched::Latest(release, etag) => {
            *ETAG.lock().ok()? = etag;
            let release = release.filter(|r| is_newer(&r.version, current));
            *AVAILABLE.lock().ok()? = release.clone();
            release
        }
    }
}

// A WinHTTP (TLS, proxykeresés) betöltve kb. 8 MB-ot és két szálat hagyna a NapCat-ben, ezért a
// hálózati munkát egy rövid életű segédfolyamat (ugyanez az exe) végzi, ami utána kilép.

/// A segédfolyamat belépési pontja: Some(kilépési kód), ha az argumentumok updater parancsot kérnek.
/// Kimenet soronként, tabulátorral: "etag <érték>", "unchanged", "release <verzió> <setup> <sha256>",
/// letöltésnél a telepítő útvonala.
pub fn run_cli(args: &[String]) -> Option<i32> {
    use std::io::Write;
    let mut out = String::new();
    let code = match args.get(1).map(String::as_str) {
        Some("--update-check") => {
            let etag = args.get(2).filter(|e| !e.is_empty()).map(String::as_str);
            match fetch(REPO, etag) {
                Some(Fetched::Unchanged) => out.push_str("unchanged\n"),
                Some(Fetched::Latest(release, etag)) => {
                    if let Some(e) = etag {
                        out.push_str(&format!("etag\t{e}\n"));
                    }
                    if let Some(r) = release {
                        out.push_str(&format!("release\t{}\t{}\t{}\n", r.version, r.setup_url, r.sha256_url));
                    }
                }
                None => {}
            }
            0
        }
        Some("--update-download") => {
            let [version, setup_url, sha256_url] = [2, 3, 4].map(|i| args.get(i).cloned().unwrap_or_default());
            let prefix = format!("https://github.com/{REPO}/releases/download/");
            let release = Release { version, setup_url, sha256_url };
            match (release.setup_url.starts_with(&prefix) && release.sha256_url.starts_with(&prefix)).then(|| download(&release)).flatten() {
                Some(path) => {
                    out.push_str(&path.to_string_lossy());
                    0
                }
                None => 1,
            }
        }
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

/// Ellenőrzés segédfolyamatban; az eredményt (ETag, elérhető kiadás) ebben a folyamatban tárolja.
pub fn check_in_child(current: &str) -> Option<Release> {
    let etag = ETAG.lock().ok()?.clone().unwrap_or_default();
    let out = run_child(&["--update-check", &etag])?;
    let mut release = None;
    for line in out.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["unchanged"] => return AVAILABLE.lock().ok()?.clone(),
            ["etag", e] => *ETAG.lock().ok()? = Some(e.to_string()),
            ["release", v, setup, sha] => {
                release = Some(Release { version: v.to_string(), setup_url: setup.to_string(), sha256_url: sha.to_string() })
            }
            _ => {}
        }
    }
    let release = release.filter(|r| is_newer(&r.version, current));
    *AVAILABLE.lock().ok()? = release.clone();
    release
}

/// Letöltés és ellenőrzés segédfolyamatban; az ellenőrzött telepítő útvonala.
pub fn download_in_child(release: &Release) -> Option<PathBuf> {
    let out = run_child(&["--update-download", &release.version, &release.setup_url, &release.sha256_url])?;
    let path = PathBuf::from(out.trim());
    path.is_file().then_some(path)
}

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

/// Letölti és ellenőrzi a telepítőt; az ellenőrzött fájl útvonala, vagy None.
pub fn download(release: &Release) -> Option<PathBuf> {
    let mut sum_text = Vec::new();
    let r = get(&release.sha256_url, "", |c| {
        sum_text.extend_from_slice(c);
        sum_text.len() < 4096
    })?;
    if r.status != 200 {
        return None;
    }
    // "<hash>  <fájlnév>" vagy csak a hash.
    let expected = String::from_utf8_lossy(&sum_text).split_whitespace().next()?.to_ascii_lowercase();

    let mut data = Vec::new();
    let r = get(&release.setup_url, "", |c| {
        data.extend_from_slice(c);
        data.len() < 64 * 1024 * 1024
    })?;
    if r.status != 200 || hex(&sha256(&data)) != expected {
        return None;
    }
    let path = std::env::temp_dir().join(format!("NapCat-Setup-{}.exe", release.version));
    std::fs::write(&path, &data).ok()?;
    Some(path)
}

/// A telepítő indítása csendes módban; utána az app kilép, a telepítő újraindítja.
pub fn run_installer(path: &PathBuf) -> bool {
    std::process::Command::new(path).args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]).spawn().is_ok()
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
    fn release_json() {
        let json = r#"{"tag_name": "v0.2.0", "name": "NapCat 0.2.0", "assets": [
            {"name": "NapCat.exe", "browser_download_url": "https://github.com/catninth/napcat/releases/download/v0.2.0/NapCat.exe"},
            {"name": "NapCat-Setup-0.2.0.exe", "browser_download_url": "https:\/\/github.com\/catninth\/napcat\/releases\/download\/v0.2.0\/NapCat-Setup-0.2.0.exe"},
            {"name": "NapCat-Setup-0.2.0.exe.sha256", "browser_download_url": "https://github.com/catninth/napcat/releases/download/v0.2.0/NapCat-Setup-0.2.0.exe.sha256"}
        ]}"#;
        let r = parse_release(json, "catninth/napcat").unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.setup_url.ends_with("/v0.2.0/NapCat-Setup-0.2.0.exe"));
        assert!(r.sha256_url.ends_with(".sha256"));
        // Más repóból származó asset nem fogadható el.
        let foreign = json.replace("catninth\\/napcat", "evil\\/napcat").replace("catninth/napcat", "evil/napcat");
        assert_eq!(parse_release(&foreign, "catninth/napcat"), None);
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// Valódi hálózati kérés egy már kiadásokkal rendelkező nyilvános repóra (csak olvasás):
    /// `cargo test live_check -- --ignored --nocapture`.
    #[test]
    #[ignore = "hálózatot használ"]
    fn live_check() {
        let mut body = Vec::new();
        let r = get("https://api.github.com/repos/catninth/clipcat/releases/latest", "Accept: application/vnd.github+json\r\n", |c| {
            body.extend_from_slice(c);
            true
        })
        .expect("kérés");
        let text = String::from_utf8_lossy(&body);
        println!("status {} etag {:?} tag {:?}", r.status, r.etag, json_strings(&text, "tag_name").first());
        println!("assets: {:?}", json_strings(&text, "name").iter().skip(1).take(6).collect::<Vec<_>>());
        println!("napcat: {:?}", check(REPO, "0.0.1"));

        // Release asset letöltése: a github.com -> CDN átirányítást a WinHTTP-nek követnie kell.
        let asset = json_strings(&text, "browser_download_url").into_iter().find(|u| u.ends_with(".sig")).expect("asset");
        let mut data = Vec::new();
        let r = get(&asset, "", |c| {
            data.extend_from_slice(c);
            true
        })
        .expect("letöltés");
        println!("asset {asset}: status {} {} bájt, sha256 {}", r.status, data.len(), hex(&sha256(&data)));
    }
}
