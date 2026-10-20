//! Locates the primary taskbar and the Widgets (weather) button via UI Automation.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
    TreeScope_Descendants, UIA_AutomationIdPropertyId,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetClassNameW, GetForegroundWindow, GetWindowRect, IsWindowVisible,
};
use windows::core::{Result, w};

const WIDGETS_AUTOMATION_ID: &str = "WidgetsButton";
/// Gap between the Widgets button and the readout, in 96-DPI pixels.
const GAP_LOGICAL: i32 = 6;
/// Offset from the taskbar's left edge when the Widgets button cannot be found.
const FALLBACK_OFFSET_LOGICAL: i32 = 12;
/// A taskbar whose top is this close to the monitor bottom is considered auto-hidden.
const HIDDEN_SLACK_PX: i32 = 4;

/// Where the overlay should be drawn, in physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub left: i32,
    pub top: i32,
    pub height: i32,
    pub dpi: u32,
}

pub struct TaskbarLocator {
    automation: IUIAutomation,
    condition: IUIAutomationCondition,
    widgets: Option<IUIAutomationElement>,
}

impl TaskbarLocator {
    pub fn new() -> Result<Self> {
        // SAFETY: COM is initialized (STA) on the UI thread before this is called.
        unsafe {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let condition = automation.CreatePropertyCondition(
                UIA_AutomationIdPropertyId,
                &VARIANT::from(WIDGETS_AUTOMATION_ID),
            )?;
            Ok(Self {
                automation,
                condition,
                widgets: None,
            })
        }
    }

    /// Drops cached UIA elements (e.g. after explorer restarted).
    pub fn invalidate(&mut self) {
        self.widgets = None;
    }

    /// Computes the overlay anchor, or `None` when the taskbar is hidden or covered.
    pub fn locate(&mut self) -> Option<Anchor> {
        // SAFETY: plain Win32 queries on a window handle that may vanish; failures map to None.
        unsafe {
            let tray = FindWindowW(w!("Shell_TrayWnd"), None).ok()?;
            if !IsWindowVisible(tray).as_bool() || fullscreen_app_active(tray) {
                return None;
            }
            let mut bar = RECT::default();
            GetWindowRect(tray, &mut bar).ok()?;
            let monitor = MonitorFromWindow(tray, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info).as_bool()
                && bar.top >= info.rcMonitor.bottom - HIDDEN_SLACK_PX
            {
                return None;
            }
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            let left = match self.widgets_rect(tray) {
                Some(button) if button.left >= bar.left && button.right <= bar.right => {
                    button.right + scale(GAP_LOGICAL, dpi_x)
                }
                _ => bar.left + scale(FALLBACK_OFFSET_LOGICAL, dpi_x),
            };
            Some(Anchor {
                left,
                top: bar.top,
                height: bar.bottom - bar.top,
                dpi: dpi_x,
            })
        }
    }

    fn widgets_rect(&mut self, tray: HWND) -> Option<RECT> {
        if let Some(rect) = self.widgets.as_ref().and_then(non_empty_rect) {
            return Some(rect);
        }
        // SAFETY: UIA calls on the taskbar's element tree.
        let found = unsafe {
            self.automation
                .ElementFromHandle(tray)
                .and_then(|root| root.FindFirst(TreeScope_Descendants, &self.condition))
                .ok()
        };
        self.widgets = found;
        self.widgets.as_ref().and_then(non_empty_rect)
    }
}

fn non_empty_rect(element: &IUIAutomationElement) -> Option<RECT> {
    // SAFETY: property read on a live (or stale, then erroring) UIA element.
    let rect = unsafe { element.CurrentBoundingRectangle() }.ok()?;
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

/// True when the foreground window covers the whole taskbar monitor (games, video, slides).
unsafe fn fullscreen_app_active(tray: HWND) -> bool {
    // SAFETY: read-only queries; any failure means "not fullscreen".
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() || foreground == tray || is_shell_window(foreground) {
            return false;
        }
        let monitor = MonitorFromWindow(tray, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut rect = RECT::default();
        if !GetMonitorInfoW(monitor, &mut info).as_bool()
            || GetWindowRect(foreground, &mut rect).is_err()
        {
            return false;
        }
        let m = info.rcMonitor;
        rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }
}

unsafe fn is_shell_window(hwnd: HWND) -> bool {
    let mut buffer = [0u16; 64];
    // SAFETY: buffer is valid for its length.
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    let class = String::from_utf16_lossy(&buffer[..len.max(0) as usize]);
    matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd")
}

pub fn scale(logical: i32, dpi: u32) -> i32 {
    logical * dpi as i32 / 96
}
