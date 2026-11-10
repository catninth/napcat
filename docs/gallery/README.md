# NapCat feature gallery

Real NapCat UI, in the app's Hungarian interface, with the same linked, two-column layout as the
ClipCat and GitCat galleries. Click any image to open the full-resolution PNG.

The flyout panel, tooltip and settings window are drawn by NapCat's own Direct2D code into an
offscreen bitmap at 2× resolution (192 DPI), so they show the window contents without the Windows
frame and title bar. Settings pages are captured at their full scrolling height. Tray icons are the
real 32 px icons a 200% taskbar uses, enlarged 2× pixel for pixel. The notifications are real
Windows toasts sent by NapCat and cropped from the screen at 100% scale.

The devices are simulated: a VGN Dragonfly F1 at 50%, an MCHOSE A7 V2 Ultra+ charging, an RK R75
Pro keyboard at 9%, a DualShock 4, a Logitech G435 with an unknown level and a disconnected Xbox
controller. The accent color is fixed to the design blue so the images don't depend on the
Windows accent color. The interface is NapCat v0.1.1 at commit `5905a55`. To recreate the images,
run `cargo test --release ui::gallery -- --ignored --test-threads=1` (the toast capture shows two
notifications on screen and expects the dark Windows theme).

[Flyout panel](#flyout-panel) · [Tooltip](#tooltip) · [Settings](#settings) · [Devices](#devices) · [Tray icons](#tray-icons) · [Notifications](#notifications)

## Flyout panel

<table>
  <tr>
    <td width="50%" valign="top">
      <a href="flyout/list-dark.png"><img src="flyout/list-dark.png" alt="NapCat flyout panel, list view, dark theme" width="100%"></a><br>
      <strong>List view</strong><br>
      Left-click a tray icon: every device with its level, charging state and time estimate, plus a low-battery alert at the top.
    </td>
    <td width="50%" valign="top">
      <a href="flyout/list-light.png"><img src="flyout/list-light.png" alt="NapCat flyout panel, list view, light theme" width="100%"></a><br>
      <strong>Light theme</strong><br>
      The panel follows the taskbar theme, or the theme chosen in Settings.
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <a href="flyout/grid-dark.png"><img src="flyout/grid-dark.png" alt="NapCat flyout panel, grid view, dark theme" width="100%"></a><br>
      <strong>Grid view</strong><br>
      Larger rings in tiles, switched with the button next to the settings gear.
    </td>
    <td width="50%" valign="top">
      <a href="flyout/grid-light.png"><img src="flyout/grid-light.png" alt="NapCat flyout panel, grid view, light theme" width="100%"></a><br>
      <strong>Grid view, light</strong><br>
      Disconnected devices keep a dashed ring and their last-seen time.
    </td>
  </tr>
</table>

## Tooltip

<table>
  <tr>
    <td width="50%" valign="top">
      <a href="tooltip/dark.png"><img src="tooltip/dark.png" alt="NapCat rich tooltip, dark theme" width="100%"></a><br>
      <strong>Rich tooltip</strong><br>
      Hover a tray icon to see every device at a glance, without clicking.
    </td>
    <td width="50%" valign="top">
      <a href="tooltip/light.png"><img src="tooltip/light.png" alt="NapCat rich tooltip, light theme" width="100%"></a><br>
      <strong>Light theme</strong><br>
      Low levels are highlighted in red; unknown levels show a dash.
    </td>
  </tr>
</table>

## Settings

<table>
  <tr>
    <td width="50%" valign="top">
      <a href="settings/general-dark.png"><img src="settings/general-dark.png" alt="NapCat settings window, dark theme" width="100%"></a><br>
      <strong>Settings</strong><br>
      Startup, tray mode, icon style, theme, accent color, per-device tray icons, notifications and refresh rate.
    </td>
    <td width="50%" valign="top">
      <a href="settings/general-light.png"><img src="settings/general-light.png" alt="NapCat settings window, light theme" width="100%"></a><br>
      <strong>Light theme</strong><br>
      Every change applies immediately; tray icons can be reordered by dragging.
    </td>
  </tr>
</table>

## Devices

<table>
  <tr>
    <td width="50%" valign="top">
      <a href="settings/devices-dark.png"><img src="settings/devices-dark.png" alt="NapCat devices page, dark theme" width="100%"></a><br>
      <strong>Devices page</strong><br>
      Remove devices, override their type, and add USB devices Windows sees but NapCat does not support yet.
    </td>
    <td width="50%" valign="top">
      <a href="settings/devices-light.png"><img src="settings/devices-light.png" alt="NapCat devices page, light theme" width="100%"></a><br>
      <strong>Light theme</strong><br>
      Unsupported devices show their USB ID and connection state only; Bluetooth pairing opens Windows settings.
    </td>
  </tr>
</table>

## Tray icons

<table>
  <tr>
    <td width="50%" valign="top">
      <a href="tray/icon-styles.png"><img src="tray/icon-styles.png" alt="NapCat tray icon styles on dark and light taskbars" width="100%"></a><br>
      <strong>Icon styles</strong><br>
      Ring, percent, badge and bar styles for 50%, charging, 9%, 72%, unknown, full and disconnected devices.
    </td>
    <td width="50%" valign="top">
      <a href="tray/app-icon-states.png"><img src="tray/app-icon-states.png" alt="NapCat app icon states" width="100%"></a><br>
      <strong>App icon</strong><br>
      Dark, light, and green when every connected device is fully charged.
    </td>
  </tr>
</table>

## Notifications

<table>
  <tr>
    <td width="50%" valign="top">
      <a href="notifications/low-battery.png"><img src="notifications/low-battery.png" alt="NapCat low-battery notification" width="100%"></a><br>
      <strong>Low battery</strong><br>
      At the configured threshold, with a 30-minute snooze; held back while a full-screen app is running.
    </td>
    <td width="50%" valign="top">
      <a href="notifications/fully-charged.png"><img src="notifications/fully-charged.png" alt="NapCat fully charged notification" width="100%"></a><br>
      <strong>Fully charged</strong><br>
      Tells you when a device can come off the charger.
    </td>
  </tr>
</table>
