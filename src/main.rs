//! Live microphone level (dBFS) for every capture device, drawn on the Windows 11 taskbar
//! right of the Widgets (weather) button.

#![windows_subsystem = "windows"]

mod alarm;
mod audio;
mod autostart;
mod config;
mod devices;
mod gdi;
mod history;
mod overlay;
mod popup;
mod readouts;
mod taskbar;

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Media::Audio::IMMDeviceEnumerator;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, MSG,
    PostMessageW, PostQuitMessage, RegisterWindowMessageW, SetTimer, TranslateMessage, WM_CLOSE,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_SETTINGCHANGE, WM_TIMER,
};
use windows::core::{Result, w};

use config::Config;
use overlay::Overlay;
use popup::Popup;
use readouts::Readouts;
use taskbar::TaskbarLocator;

const TICK_MS: u32 = 100;
/// Re-locate the Widgets button and re-assert topmost every N ticks (1 s).
const RELOCATE_EVERY: u32 = 10;
/// Re-enumerate capture devices every N ticks (3 s).
const DEVICES_EVERY: u32 = 30;
const TIMER_ID: usize = 1;
/// The cursor must rest on the readout this many ticks (300 ms) before the chart opens.
const HOVER_SHOW_TICKS: u32 = 3;
/// Redraw the open chart every N ticks (2 Hz, the history sample rate).
const CHART_REFRESH_EVERY: u32 = 5;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

struct App {
    config: Config,
    enumerator: IMMDeviceEnumerator,
    readouts: Readouts,
    locator: TaskbarLocator,
    overlay: Overlay,
    popup: Popup,
    anchor: Option<taskbar::Anchor>,
    ticks: u32,
    hover_ticks: u32,
}

impl App {
    fn new(overlay: Overlay, popup: Popup) -> Result<Self> {
        let mut app = Self {
            config: Config::load(),
            enumerator: devices::enumerator()?,
            readouts: Readouts::default(),
            locator: TaskbarLocator::new()?,
            overlay,
            popup,
            anchor: None,
            ticks: 0,
            hover_ticks: 0,
        };
        app.sync_devices();
        app.relocate();
        Ok(app)
    }

    fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(DEVICES_EVERY) {
            self.sync_devices();
        }
        if self.ticks.is_multiple_of(RELOCATE_EVERY) {
            self.relocate();
        }
        let segments = self.readouts.tick(self.config.offset_db, Instant::now());
        // A failed draw is retried on the next tick; nothing useful to report to the user.
        let _ = self.overlay.show(&segments, self.anchor);
        self.update_popup();
    }

    /// Shows the history chart while the cursor rests on the (click-through) readout.
    fn update_popup(&mut self) {
        let target = self.overlay.rect().zip(self.anchor);
        let hovered = target.is_some_and(|(rect, _)| cursor_inside(rect));
        self.hover_ticks = if hovered {
            self.hover_ticks.saturating_add(1)
        } else {
            0
        };
        let Some((rect, anchor)) = target.filter(|_| self.hover_ticks >= HOVER_SHOW_TICKS) else {
            self.popup.hide();
            return;
        };
        if !self.popup.is_visible() || self.ticks.is_multiple_of(CHART_REFRESH_EVERY) {
            let _ = self.popup.show(&self.readouts.series(), rect, anchor.dpi);
        }
    }

    fn sync_devices(&mut self) {
        // Keep the previous list on transient enumeration errors.
        if let Ok(list) = devices::active_capture_devices(&self.enumerator) {
            self.readouts.sync(&list, &self.config);
        }
    }

    fn relocate(&mut self) {
        self.anchor = self.locator.locate();
        self.overlay.reassert_topmost();
    }

    fn on_taskbar_recreated(&mut self) {
        self.locator.invalidate();
        self.relocate();
    }
}

fn cursor_inside(rect: RECT) -> bool {
    let mut cursor = POINT::default();
    // SAFETY: writes the cursor position into a local.
    unsafe { GetCursorPos(&mut cursor) }.is_ok()
        && (rect.left..rect.right).contains(&cursor.x)
        && (rect.top..rect.bottom).contains(&cursor.y)
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut()
            && let Some(app) = slot.as_mut()
        {
            f(app);
        }
    });
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed);
    match msg {
        WM_TIMER => with_app(App::tick),
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => with_app(App::relocate),
        m if m != 0 && m == taskbar_created => with_app(App::on_taskbar_recreated),
        WM_CLOSE => {
            // SAFETY: destroying our own window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

enum Command {
    Run,
    Autostart(bool),
    Stop,
    List,
    Help,
}

fn parse_args() -> std::result::Result<Command, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => Ok(Command::Run),
        ["--autostart", "on"] => Ok(Command::Autostart(true)),
        ["--autostart", "off"] => Ok(Command::Autostart(false)),
        ["--stop"] => Ok(Command::Stop),
        ["--list"] => Ok(Command::List),
        ["--help" | "-h"] => Ok(Command::Help),
        other => Err(format!("unrecognised arguments: {}", other.join(" "))),
    }
}

const USAGE: &str = "mic-db-taskbar [--autostart on|off] [--stop] [--help]
  (no args)          run the overlay (single instance)
  --autostart on|off add/remove the HKCU Run entry for this exe
  --stop             close a running instance
  --list             list capture devices and whether they would be shown";

fn main() {
    let command = parse_args();
    if !matches!(command, Ok(Command::Run)) {
        // SAFETY: lets CLI output reach the parent console of a GUI-subsystem exe.
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    let outcome = match command {
        Ok(Command::Run) => run(),
        Ok(Command::Autostart(true)) => autostart::enable().map(|()| println!("autostart: on")),
        Ok(Command::Autostart(false)) => autostart::disable().map(|()| println!("autostart: off")),
        Ok(Command::Stop) => {
            stop_running_instance();
            Ok(())
        }
        Ok(Command::List) => list_devices(),
        Ok(Command::Help) => {
            println!("{USAGE}");
            Ok(())
        }
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(error) = outcome {
        eprintln!("mic-db-taskbar: {error}");
        std::process::exit(1);
    }
}

fn list_devices() -> Result<()> {
    // SAFETY: COM for this short-lived CLI thread.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let config = Config::load();
    for device in devices::active_capture_devices(&devices::enumerator()?)? {
        let shown = if config.is_excluded(&device.friendly_name, device.bluetooth) {
            "skipped"
        } else {
            "shown"
        };
        let bus = if device.bluetooth {
            "bluetooth"
        } else {
            "wired"
        };
        println!(
            "{shown:<8} {bus:<9} {} [{}]",
            device.friendly_name, device.device_path
        );
    }
    Ok(())
}

fn stop_running_instance() {
    // SAFETY: posts WM_CLOSE to our own overlay window class, if present.
    unsafe {
        match FindWindowW(overlay::WINDOW_CLASS, None) {
            Ok(hwnd) => {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                println!("stopped");
            }
            Err(_) => println!("not running"),
        }
    }
}

fn run() -> Result<()> {
    // SAFETY: process-wide setup calls made once at startup on the main thread.
    unsafe {
        let _instance = CreateMutexW(None, true, w!("Local\\mic-db-taskbar.single-instance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );

        let overlay = Overlay::create(Some(wndproc))?;
        let hwnd = overlay.hwnd();
        let app = App::new(overlay, Popup::create()?)?;
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        if SetTimer(Some(hwnd), TIMER_ID, TICK_MS, None) == 0 {
            return Err(windows::core::Error::from_thread());
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // Stop capture threads before COM goes away.
        APP.with(|cell| cell.borrow_mut().take());
        Ok(())
    }
}
