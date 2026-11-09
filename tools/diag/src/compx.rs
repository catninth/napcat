//! Compx vevő (VGN / ATK / VXE egerek) akkulekérdezése, az ATK HUB webes driver protokollja szerint.
//!
//! Keret: 16 bájt a 8-as reportban: [parancs, állapot, cím (2), hossz, adat (10), ellenőrzőösszeg],
//! az összeg 0x55 - (8 + az első 15 bájt összege). Csak a GetBatteryLevel (0x04) olvasó parancsot
//! küldi; a válasz ugyanabban a reportban jön, első bájtja a parancs.

use crate::io::{hex, Dev};

const HANDSHAKE: u8 = 0x01; // DownLoadData: 4 véletlen bájt, válasz: titkosított bájtok, CID, MID, kapcsolattípus
const GET_MOUSE_ONLINE: u8 = 0x03;
const GET_BATTERY_LEVEL: u8 = 0x04;

pub fn frame(cmd: u8, data: &[u8]) -> [u8; 17] {
    let mut f = [0u8; 17];
    f[0] = 0x08; // report ID
    f[1] = cmd;
    f[5] = data.len() as u8;
    f[6..6 + data.len()].copy_from_slice(data);
    let sum: u32 = 8 + f[1..16].iter().map(|&b| b as u32).sum::<u32>();
    f[16] = 0x55u8.wrapping_sub((sum & 0xFF) as u8);
    f
}

pub unsafe fn run() {
    let devs: Vec<_> = crate::all_hid_info()
        .into_iter()
        .filter(|d| d.vid == 0x3554 && d.page == 0xFF02 && d.usage == 0x0002 && d.in_len == 17 && d.out_len == 17)
        .collect();
    if devs.is_empty() {
        println!("Nincs Compx vevő (0xFF02/0x0002, 17 bájtos reportok).");
        return;
    }
    for d in devs {
        println!("=== {:04X}:{:04X} \"{}\"", d.vid, d.pid, d.product);
        let Some(dev) = Dev::open(&d.path) else {
            println!("  nem nyitható meg");
            continue;
        };
        // A többi gyártói csatornát is figyeljük, hátha ott jön a válasz.
        let others: Vec<_> = crate::all_hid_info()
            .into_iter()
            .filter(|o| o.vid == d.vid && o.pid == d.pid && o.page >= 0xFF00 && o.in_len > 0 && o.path != d.path)
            .filter_map(|o| Some((o.page, o.in_len, Dev::open(&o.path)?)))
            .collect();
        // Az egér alvás közben nem válaszol: várunk, amíg online lesz (mozgatás ébreszti).
        let mut buf = [0u8; 17];
        let mut online = false;
        for _ in 0..30 {
            let req = frame(GET_MOUSE_ONLINE, &[]);
            if dev.write(&req, 500) {
                if let Some(n) = dev.read(&mut buf, 500) {
                    if n >= 7 && buf[1] == GET_MOUSE_ONLINE && buf[6] != 0 {
                        println!("  egér online: {}", hex(&buf[..n]));
                        online = true;
                        break;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
        if !online {
            println!("  az egér 30 mp alatt nem lett online (alszik?)");
        }
        let seed = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()).to_le_bytes();
        let steps: [(&str, [u8; 17]); 3] = [
            ("online?", frame(GET_MOUSE_ONLINE, &[])),
            ("kézfogás", frame(HANDSHAKE, &[seed[0], seed[1], seed[2], seed[3], 0, 0, 0, 0])),
            ("akku", frame(GET_BATTERY_LEVEL, &[])),
        ];
        for (name, req) in steps {
            println!("  {name} kérés:  {}", hex(&req));
            let mut buf = [0u8; 17];
            'retry: for attempt in 1..=3 {
                if !dev.write(&req, 1000) {
                    println!("  az írás nem sikerült");
                    break;
                }
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(600);
                while std::time::Instant::now() < deadline {
                    if let Some(n) = dev.read(&mut buf, 50) {
                        let r = &buf[..n];
                        println!("  {name} válasz: {}", hex(r));
                        if r.len() >= 10 && r[0] == 0x08 && r[1] == req[1] {
                            if req[1] == GET_BATTERY_LEVEL {
                                let (status, level, charge) = (r[2], r[6], r[7]);
                                let mv = u16::from_be_bytes([r[8], r[9]]);
                                println!("  állapot {status}, szint {level}%, töltés {charge}, feszültség {mv} mV");
                            }
                            break 'retry;
                        }
                    }
                    for (page, len, o) in &others {
                        let mut ob = vec![0u8; *len as usize];
                        if let Some(n) = o.read(&mut ob, 5) {
                            println!("  (0x{page:04X}) {}", hex(&ob[..n]));
                        }
                    }
                }
                println!("  {name}: {attempt}. próbálkozás, nincs válasz");
            }
        }
    }
}
