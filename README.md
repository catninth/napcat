# NapCat

Battery levels of your wireless devices, right in the Windows taskbar.

Each device gets its own tray icon: the device glyph surrounded by a ring that shows the charge
level, with a lightning badge while charging. Written in Rust on plain Win32, Direct2D and WinRT:
no WebView, no UI framework, and practically zero CPU when idle.

> Early development. Windows 10 and 11.

## Status

| Milestone | |
|---|---|
| M0 – tray icon, single instance, EcoQoS | done |
| M1 – ring icon rendering (Direct2D) | done |
| Device thread + cable/dongle detection | done |
| Installer (Inno Setup) + release workflow | done, not yet released |
| Flyout panel (left click), tray menu, hide devices, autostart, low battery notifications | done |
| PlayStation controllers (DualSense, DualSense Edge, DualShock 4; USB and Bluetooth) | done, untested on hardware |
| Xbox controllers (XInput, approximate level) | done, untested on hardware |
| Auto-update | planned |
| Bluetooth battery, Logitech HID++, headsets, VGN, … | planned |

## Building

Requires the Rust MSVC toolchain.

```sh
cargo build --release
cargo test preview      # renders all icon states to target/icon-preview.png
iscc installer\napcat.iss   # installer -> target\installer (Inno Setup 6)
```

Releases are built by `.github/workflows/release.yml` when a `v*` tag is pushed; the tag must match
the version in `Cargo.toml`.

## Tools

- `tools/diag` – lists HID interfaces, Bluetooth battery properties and standard HID battery
  usages; `via` sends read-only VIA queries, `listen` passively logs vendor-defined reports.
- `tools/webhid-logger.js` – paste into the DevTools console of a vendor's web driver to log its
  WebHID traffic (used to find battery commands).
