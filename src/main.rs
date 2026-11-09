#![windows_subsystem = "windows"]

mod model;
mod ui;

use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{w, Result};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;

fn main() -> Result<()> {
    unsafe {
        // Egypéldányos futás: a mutex a folyamat végéig él.
        let _mutex = CreateMutexW(None, true, w!("Local\\NapCat.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
        enable_eco_qos();
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?; // WIC-hez
        let hinst = GetModuleHandleW(None)?;
        ui::tray::run(hinst.into())
    }
}

/// EcoQoS / Efficiency mode: Windows 11-en a takarékos magokon fut, Win10-en kisebb hatással.
unsafe fn enable_eco_qos() {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    let _ = SetProcessInformation(
        GetCurrentProcess(),
        ProcessPowerThrottling,
        &state as *const _ as *const c_void,
        size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
    );
}
