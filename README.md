# mic-db-taskbar

A tiny Windows 11 app that shows the live level of every microphone, in dBFS, right on the
taskbar, immediately to the right of the Widgets (weather) button.

![screenshot](docs/screenshot.png)

- One readout per active capture device, side by side: `USB −42 dB │ Intel Smart… −61 dB`.
- Level = RMS over 100 ms windows from a WASAPI shared-mode capture stream per device,
  smoothed (fast attack, slow release), refreshed at 10 Hz, floored at −90 dB.
- Devices are re-enumerated every 3 s, so plugging/unplugging a mic just works.
- Click-through, borderless, topmost overlay; per-monitor DPI aware; follows the light/dark
  system theme; hides itself when a fullscreen app is in front or the taskbar auto-hides.
- Single small exe, no console, no tray icon, ~0.5 % of one core.

## Install

Download `mic-db-taskbar.exe` from the
[latest release](https://github.com/LautaroLasorsa/mic-db-taskbar/releases/latest), put it
somewhere permanent (e.g. `%LOCALAPPDATA%\Programs\mic-db-taskbar\`) and run it.

Or build it yourself (Rust 1.88+):

```powershell
cargo install --git https://github.com/LautaroLasorsa/mic-db-taskbar
mic-db-taskbar
```

## Command line

```text
mic-db-taskbar                  run the overlay (only one instance runs)
mic-db-taskbar --autostart on   start with Windows (HKCU\...\CurrentVersion\Run)
mic-db-taskbar --autostart off  remove the autostart entry
mic-db-taskbar --stop           close the running instance
```

`--autostart on` registers the path of the exe you run it from, so run it from the installed
location.

## Configuration (optional)

Create `mic-db-taskbar.toml` next to the exe, or `%APPDATA%\mic-db-taskbar\config.toml`.
It is read at startup (`--stop` and start again to apply changes).

```toml
# Calibration added to every reading, in dB.
offset_db = 0

# Short labels: "substring of the device's friendly name" = "label" (case-insensitive).
[labels]
"Realtek" = "Int"
"REDRAGON" = "Headset"
```

Without a label, the name inside the parentheses of the Windows device name is used
(`Microphone (USB Audio Device)` → `USB`), truncated to 12 characters.

## Notes and limitations

- Windows shows the "microphone in use" indicator while the app runs, because it really
  opens a capture stream on each mic (the peak meter API reads 0 without one).
- A device held in exclusive mode by another app shows `n/a`.
- Only the primary taskbar is used. The Widgets button is found through UI Automation
  (`AutomationId = WidgetsButton`); if it is hidden or not found, the readout sits at the
  taskbar's left edge. Designed for the default centered layout, where the left side of the
  taskbar is free; on a left-aligned taskbar it may overlap app buttons.
- The readout is an overlay, not part of explorer: it re-asserts topmost every second and
  follows explorer restarts (`TaskbarCreated`).

## License

MIT
