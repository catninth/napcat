//! Passzív figyelés: minden gyártóspecifikus (0xFF00+) input csatornát olvas, semmit nem küld.
//! Egyes vevők maguktól küldenek állapot- vagy akkujelentést.

use std::thread;
use std::time::{Duration, Instant};

use crate::io::{hex, Dev};

pub unsafe fn run(secs: u64) {
    let devs: Vec<_> = crate::all_hid_info()
        .into_iter()
        .filter(|d| d.page >= 0xFF00 && d.in_len > 0 && !d.product.contains("DFU"))
        .collect();
    println!("Figyelés {secs} mp-ig {} csatornán. Közben használd az eszközöket, dugd ki/be a kábelt stb.", devs.len());
    let start = Instant::now();
    let deadline = start + Duration::from_secs(secs);
    let handles: Vec<_> = devs
        .into_iter()
        .map(|d| {
            thread::spawn(move || {
                let tag = format!("{:04X}:{:04X} {} 0x{:04X}", d.vid, d.pid, d.product, d.page);
                let Some(dev) = Dev::open(&d.path) else {
                    println!("[{tag}] nem nyitható meg");
                    return;
                };
                let mut buf = vec![0u8; d.in_len as usize];
                while Instant::now() < deadline {
                    if let Some(n) = dev.read(&mut buf, 500) {
                        println!("{:>6.2}s [{tag}] {}", start.elapsed().as_secs_f32(), hex(&buf[..n]));
                    }
                }
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
}
