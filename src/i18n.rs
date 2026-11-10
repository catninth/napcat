//! A felület nyelve: angol (alapértelmezés) vagy magyar; "auto" esetén a Windows megjelenítési nyelvét követi (magyar
//! Windowson magyar, minden máson angol). A beállításokban választható.
//!
//! A szövegek a hívás helyén vannak, párban: `t("magyar", "English")`.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// `setting`: "auto", "hu" vagy "en".
pub fn set(setting: &str) {
    let english = match setting {
        "hu" => false,
        "auto" => !system_is_hungarian(),
        _ => true,
    };
    ENGLISH.store(english, Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// A nyelvnek megfelelő szöveg.
pub fn t<'a>(hu: &'a str, en: &'a str) -> &'a str {
    if english() { en } else { hu }
}

fn system_is_hungarian() -> bool {
    // LANG_HUNGARIAN = 0x0E (a nyelvazonosító alsó 10 bitje az elsődleges nyelv).
    let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    lang & 0x3FF == 0x0E
}

