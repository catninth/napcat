# Royal Kludge R75 Pro: battery level for NapCat

The stock R75 Pro firmware keeps the battery level to itself (it only shows it on the number row
LEDs). These patches add one read-only VIA value so NapCat can read it, over the cable and through
the 2.4G dongle (the dongle forwards VIA requests to the keyboard).

VIA request: `id_custom_get_value, id_custom_channel, id_battery` = `08 00 05`.
Reply: `08 00 05 <level %> <state>`, state 0 discharging, 1 charging, 2 full. The request does not
reset the keyboard's sleep timer. On stock firmware the reply starts with `FF` and NapCat falls back
to showing only the connection.

Verified on an R75 Pro (342D:E487, tri-mode, US): typing, 2.4G, Bluetooth and the battery reply
over cable and dongle.

## Build

1. QMK **0.28.10** (the RK sources need `EECONFIG_USER_DATABLOCK`, removed in 0.29, and the
   `RM_*` keycodes, added in 0.27).
2. The RK source "R75 Pro, Tri mode, US" from https://en.rkgaming.com/download/ (QMK tab) into
   `keyboards/royalkludge/r75pro_us_triple_mode`, then apply `r75pro_us_triple_mode.patch`.
3. RK does not publish `keyboards/royalkludge/linker/wireless`. Copy `keyboards/linker/wireless`
   from https://github.com/banroku/qmk_firmware into that path, change the path in its
   `wireless.mk` to `keyboards/royalkludge/linker/wireless`, and apply
   `linker-wireless-transport.patch` (QMK 0.28 USB protocol API).
4. `make royalkludge/r75pro_us_triple_mode:default`

The patch also gives the wireless driver's UART the QMK 0.28 pin names (`UART_DRIVER SD3`,
`UART_TX_PIN C10`, `UART_RX_PIN C11`); without them it builds but talks to the wireless module on
the wrong pins.

## Flash and restore

Bootloader: hold Fn, hold 9, press Esc (or hold Esc while plugging in the cable). This resets the
keyboard's VIA keymap and lighting settings. Read a backup first:

```sh
wb32-dfu-updater_cli --upload backup.bin --upload-size 131072   # the chip has 128 KB of flash
wb32-dfu-updater_cli -D royalkludge_r75pro_us_triple_mode_default.bin && wb32-dfu-updater_cli -R
```

Restore the stock firmware the same way with `-D backup.bin`. On Windows, copy the tool to a name
without "updater" in it, or Windows asks for administrator rights.
