//! Időkorlátos HID olvasás/írás overlapped I/O-val.

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::IO::*;

pub struct Dev {
    h: HANDLE,
    ev: HANDLE,
}

impl Dev {
    pub fn open(path: &[u16]) -> Option<Dev> {
        unsafe {
            let h = CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
            .ok()?;
            let ev = CreateEventW(None, true, false, PCWSTR::null()).ok()?;
            Some(Dev { h, ev })
        }
    }

    /// Lefuttat egy overlapped műveletet; időtúllépéskor megszakítja és megvárja a lezárást.
    unsafe fn complete(&self, ov: &mut OVERLAPPED, started: windows::core::Result<()>, timeout_ms: u32) -> Option<usize> {
        if let Err(e) = started {
            if e.code() != ERROR_IO_PENDING.to_hresult() {
                return None;
            }
        }
        let mut n = 0u32;
        if WaitForSingleObject(self.ev, timeout_ms) != WAIT_OBJECT_0 {
            let _ = CancelIoEx(self.h, Some(ov));
            let _ = GetOverlappedResult(self.h, ov, &mut n, true);
            return None;
        }
        GetOverlappedResult(self.h, ov, &mut n, false).ok()?;
        Some(n as usize)
    }

    pub fn write(&self, buf: &[u8], timeout_ms: u32) -> bool {
        unsafe {
            let _ = ResetEvent(self.ev);
            let mut ov = OVERLAPPED { hEvent: self.ev, ..Default::default() };
            let r = WriteFile(self.h, Some(buf), None, Some(&mut ov));
            self.complete(&mut ov, r, timeout_ms).is_some()
        }
    }

    pub fn read(&self, buf: &mut [u8], timeout_ms: u32) -> Option<usize> {
        unsafe {
            let _ = ResetEvent(self.ev);
            let mut ov = OVERLAPPED { hEvent: self.ev, ..Default::default() };
            let r = ReadFile(self.h, Some(buf), None, Some(&mut ov));
            self.complete(&mut ov, r, timeout_ms)
        }
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.ev);
            let _ = CloseHandle(self.h);
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}
