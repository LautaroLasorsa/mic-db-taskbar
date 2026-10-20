//! Hover popup with a line chart of the last five minutes, one line per microphone.

use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    FW_NORMAL, FW_SEMIBOLD, GetMonitorInfoW, GetStockObject, LineTo, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromRect, MoveToEx, PS_DASH, PS_SOLID, Polyline, RoundRect, SYSTEM_FONT,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};
use windows::core::{PCWSTR, Result, w};

use crate::gdi::{self, Canvas, Font, Tool, colorref};
use crate::history::SPAN;
use crate::overlay::reassert_topmost;
use crate::readouts::{Series, format_level};
use crate::taskbar::scale;

const WINDOW_CLASS: PCWSTR = w!("MicDbTaskbarChart");
const DB_MIN: f32 = -90.0;
const DB_MAX: f32 = 0.0;
const GRID_STEP_DB: i32 = 10;
const LABEL_STEP_DB: i32 = 20;
const MINUTES: i32 = (SPAN.as_secs() / 60) as i32;

/// Layout in 96-DPI pixels.
const WIDTH: i32 = 460;
const CHART_HEIGHT: i32 = 170;
const PADDING: i32 = 12;
const FONT_PX: i32 = 12;
const ROW_HEIGHT: i32 = 18;
const AXIS_LABEL_WIDTH: i32 = 30;
const X_LABEL_HEIGHT: i32 = 18;
const GAP_ABOVE_TASKBAR: i32 = 8;
const CORNER_RADIUS: i32 = 8;
const SWATCH: i32 = 10;
const LINE_WIDTH: i32 = 2;

type Rgb = (u8, u8, u8);

struct Theme {
    background: Rgb,
    border: Rgb,
    grid: Rgb,
    text: Rgb,
    muted: Rgb,
    alarm: Rgb,
    lines: [Rgb; 6],
}

const DARK: Theme = Theme {
    background: (0x20, 0x20, 0x20),
    border: (0x45, 0x45, 0x45),
    grid: (0x38, 0x38, 0x38),
    text: (0xF2, 0xF2, 0xF2),
    muted: (0x9A, 0x9A, 0x9A),
    alarm: (0xFF, 0x45, 0x3A),
    lines: [
        (0x4F, 0xC3, 0xF7),
        (0xFF, 0xB7, 0x4D),
        (0x81, 0xC7, 0x84),
        (0xF0, 0x62, 0x92),
        (0xBA, 0x68, 0xC8),
        (0xFF, 0xF1, 0x76),
    ],
};

const LIGHT: Theme = Theme {
    background: (0xF9, 0xF9, 0xF9),
    border: (0xD0, 0xD0, 0xD0),
    grid: (0xE4, 0xE4, 0xE4),
    text: (0x1A, 0x1A, 0x1A),
    muted: (0x60, 0x60, 0x60),
    alarm: (0xD0, 0x10, 0x10),
    lines: [
        (0x02, 0x77, 0xBD),
        (0xEF, 0x6C, 0x00),
        (0x2E, 0x7D, 0x32),
        (0xC2, 0x18, 0x5B),
        (0x6A, 0x1B, 0x9A),
        (0x9E, 0x9D, 0x24),
    ],
};

extern "system" fn popup_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: default handling; the popup has no interactive behaviour.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub struct Popup {
    hwnd: HWND,
    visible: bool,
}

impl Popup {
    pub fn create() -> Result<Self> {
        let hwnd = gdi::create_window(
            WINDOW_CLASS,
            Some(popup_wndproc),
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST,
        )?;
        Ok(Self {
            hwnd,
            visible: false,
        })
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Renders the chart above `readout` (the overlay's screen rect) and shows it.
    pub fn show(&mut self, series: &[Series<'_>], readout: RECT, dpi: u32) -> Result<()> {
        let theme = if gdi::is_light_theme() { &LIGHT } else { &DARK };
        let layout = Layout::new(series.len(), dpi);
        let mut canvas = Canvas::new(layout.size)?;
        draw(&canvas, &layout, theme, series, Instant::now())?;
        finish_pixels(&mut canvas, scale(CORNER_RADIUS, dpi));
        canvas.present(self.hwnd, position(readout, layout.size, dpi))?;
        if !self.visible {
            // SAFETY: valid window handle owned by this struct.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.visible = true;
        }
        reassert_topmost(self.hwnd);
        Ok(())
    }

    pub fn hide(&mut self) {
        if self.visible {
            // SAFETY: valid window handle.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
    }
}

/// Pixel geometry of the popup at a given DPI.
struct Layout {
    dpi: u32,
    size: SIZE,
    legend_top: i32,
    plot: RECT,
}

impl Layout {
    fn new(rows: usize, dpi: u32) -> Self {
        let s = |v: i32| scale(v, dpi);
        let width = s(WIDTH);
        let legend_top = s(PADDING) + s(ROW_HEIGHT);
        let plot_top = legend_top + s(ROW_HEIGHT) * rows.max(1) as i32 + s(PADDING) / 2;
        let plot = RECT {
            left: s(PADDING) + s(AXIS_LABEL_WIDTH),
            top: plot_top,
            right: width - s(PADDING),
            bottom: plot_top + s(CHART_HEIGHT),
        };
        let height = plot.bottom + s(X_LABEL_HEIGHT) + s(PADDING) / 2;
        Self {
            dpi,
            size: SIZE {
                cx: width,
                cy: height,
            },
            legend_top,
            plot,
        }
    }

    fn s(&self, logical: i32) -> i32 {
        scale(logical, self.dpi)
    }

    fn y_for(&self, db: f32) -> i32 {
        let t = ((db.clamp(DB_MIN, DB_MAX) - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0.0, 1.0);
        self.plot.bottom - (t * (self.plot.bottom - self.plot.top) as f32).round() as i32
    }

    /// `seconds_ago` in `0..=SPAN` maps right-to-left across the plot.
    fn x_for(&self, seconds_ago: f32) -> i32 {
        let t = (seconds_ago / SPAN.as_secs_f32()).clamp(0.0, 1.0);
        self.plot.right - (t * (self.plot.right - self.plot.left) as f32).round() as i32
    }
}

fn draw(
    canvas: &Canvas,
    layout: &Layout,
    theme: &Theme,
    series: &[Series<'_>],
    now: Instant,
) -> Result<()> {
    let dc = canvas.dc();
    let size = canvas.size;
    let regular = Font::new(layout.s(FONT_PX), FW_NORMAL)?;
    let bold = Font::new(layout.s(FONT_PX), FW_SEMIBOLD)?;
    // SAFETY: drawing into our own memory DC with objects that outlive each call.
    unsafe {
        SetBkMode(dc, TRANSPARENT);
        {
            let _fill = Tool::brush(dc, colorref(theme.background));
            let _edge = Tool::pen(dc, PS_SOLID, 1, colorref(theme.border));
            let r = layout.s(CORNER_RADIUS) * 2;
            let _ = RoundRect(dc, 0, 0, size.cx, size.cy, r, r);
        }
        bold.select(dc);
        text(
            dc,
            layout.s(PADDING),
            layout.s(PADDING),
            "Microphone level \u{00B7} last 5 min",
            theme.text,
        );

        regular.select(dc);
        draw_grid(dc, layout, theme);
        for (index, item) in series.iter().enumerate() {
            let color = theme.lines[index % theme.lines.len()];
            draw_legend_row(dc, layout, index, item, color, theme);
            draw_threshold(dc, layout, item.alarm_db, theme.alarm);
            draw_series(dc, layout, item, color, now);
        }
        // Deselect our fonts so they can be deleted when dropped.
        SelectObject(dc, GetStockObject(SYSTEM_FONT));
    }
    Ok(())
}

unsafe fn draw_grid(dc: windows::Win32::Graphics::Gdi::HDC, layout: &Layout, theme: &Theme) {
    let plot = layout.plot;
    let _pen = Tool::pen(dc, PS_SOLID, 1, colorref(theme.grid));
    // SAFETY: plain drawing into our DC.
    unsafe {
        for db in (DB_MIN as i32..=DB_MAX as i32).step_by(GRID_STEP_DB as usize) {
            let y = layout.y_for(db as f32);
            let _ = MoveToEx(dc, plot.left, y, None);
            let _ = LineTo(dc, plot.right, y);
            if db % LABEL_STEP_DB == 0 {
                let label = if db == 0 {
                    "0".to_owned()
                } else {
                    format!("\u{2212}{}", -db)
                };
                text(
                    dc,
                    layout.s(PADDING),
                    y - layout.s(FONT_PX) / 2 - 1,
                    &label,
                    theme.muted,
                );
            }
        }
        for minute in 0..=MINUTES {
            let x = layout.x_for((minute * 60) as f32);
            let _ = MoveToEx(dc, x, plot.top, None);
            let _ = LineTo(dc, x, plot.bottom);
            let label = if minute == 0 {
                "now".to_owned()
            } else {
                format!("\u{2212}{minute}m")
            };
            let wide = gdi::wide(&label);
            let width = gdi::text_extent(dc, &wide).cx;
            let left =
                (x - width / 2).clamp(plot.left - layout.s(AXIS_LABEL_WIDTH), plot.right - width);
            text(dc, left, plot.bottom + layout.s(3), &label, theme.muted);
        }
    }
}

unsafe fn draw_legend_row(
    dc: windows::Win32::Graphics::Gdi::HDC,
    layout: &Layout,
    index: usize,
    item: &Series<'_>,
    color: Rgb,
    theme: &Theme,
) {
    let top = layout.legend_top + layout.s(ROW_HEIGHT) * index as i32;
    let left = layout.s(PADDING);
    let swatch = layout.s(SWATCH);
    let swatch_top = top + (layout.s(ROW_HEIGHT) - swatch) / 2;
    {
        let _fill = Tool::brush(dc, colorref(color));
        let _edge = Tool::pen(dc, PS_SOLID, 1, colorref(color));
        // SAFETY: plain drawing into our DC.
        unsafe {
            let _ = RoundRect(
                dc,
                left,
                swatch_top,
                left + swatch,
                swatch_top + swatch,
                3,
                3,
            );
        }
    }
    let current = item.history.samples().last().and_then(|sample| sample.db);
    let above = current.is_some_and(|db| db > item.alarm_db);
    let x = left + swatch + layout.s(8);
    let y = top + (layout.s(ROW_HEIGHT) - layout.s(FONT_PX)) / 2 - 1;
    text(dc, x, y, item.label, theme.text);
    let value_x = x + layout.s(110);
    text(
        dc,
        value_x,
        y,
        &format_level(current),
        if above { theme.alarm } else { theme.text },
    );
    let limit = format!("alarm > {}", format_level(Some(item.alarm_db)));
    text(dc, value_x + layout.s(70), y, &limit, theme.muted);
}

unsafe fn draw_threshold(
    dc: windows::Win32::Graphics::Gdi::HDC,
    layout: &Layout,
    alarm_db: f32,
    color: Rgb,
) {
    if !(DB_MIN..=DB_MAX).contains(&alarm_db) {
        return;
    }
    let y = layout.y_for(alarm_db);
    // Dashed pens must be 1 px wide in GDI.
    let _pen = Tool::pen(dc, PS_DASH, 1, colorref(color));
    // SAFETY: plain drawing into our DC.
    unsafe {
        let _ = MoveToEx(dc, layout.plot.left, y, None);
        let _ = LineTo(dc, layout.plot.right, y);
    }
}

unsafe fn draw_series(
    dc: windows::Win32::Graphics::Gdi::HDC,
    layout: &Layout,
    item: &Series<'_>,
    color: Rgb,
    now: Instant,
) {
    let _pen = Tool::pen(dc, PS_SOLID, layout.s(LINE_WIDTH).max(1), colorref(color));
    let mut run: Vec<POINT> = Vec::new();
    let flush = |run: &mut Vec<POINT>| {
        if run.len() >= 2 {
            // SAFETY: plain drawing into our DC.
            unsafe {
                let _ = Polyline(dc, run);
            }
        }
        run.clear();
    };
    for sample in item.history.samples() {
        let ago = now.saturating_duration_since(sample.at).as_secs_f32();
        match sample.db {
            Some(db) => run.push(POINT {
                x: layout.x_for(ago),
                y: layout.y_for(db),
            }),
            None => flush(&mut run),
        }
    }
    flush(&mut run);
}

fn text(dc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, value: &str, color: Rgb) {
    let wide = gdi::wide(value);
    // SAFETY: plain text output into our DC.
    unsafe {
        SetTextColor(dc, colorref(color));
        let _ = TextOutW(dc, x, y, &wide);
    }
}

/// GDI leaves alpha at 0: make the body opaque and cut the rounded corners out.
fn finish_pixels(canvas: &mut Canvas, radius: i32) {
    let SIZE {
        cx: width,
        cy: height,
    } = canvas.size;
    for (index, pixel) in canvas.pixels().iter_mut().enumerate() {
        let (x, y) = ((index as i32) % width, (index as i32) / width);
        *pixel = if outside_rounded_rect(x, y, width, height, radius) {
            0
        } else {
            *pixel | 0xFF00_0000
        };
    }
}

fn outside_rounded_rect(x: i32, y: i32, width: i32, height: i32, radius: i32) -> bool {
    let cx = if x < radius {
        radius
    } else if x >= width - radius {
        width - radius - 1
    } else {
        return false;
    };
    let cy = if y < radius {
        radius
    } else if y >= height - radius {
        height - radius - 1
    } else {
        return false;
    };
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy > radius * radius
}

/// Places the popup above the readout, clamped to the monitor's work area.
fn position(readout: RECT, size: SIZE, dpi: u32) -> POINT {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: read-only monitor query.
    let work = unsafe {
        let monitor = MonitorFromRect(&readout, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            readout
        }
    };
    let bottom = readout.top.min(work.bottom) - scale(GAP_ABOVE_TASKBAR, dpi);
    let x = readout.left.min(work.right - size.cx).max(work.left);
    let y = (bottom - size.cy).max(work.top);
    POINT { x, y }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_map_range_to_plot_edges() {
        let layout = Layout::new(2, 96);
        assert_eq!(layout.y_for(0.0), layout.plot.top);
        assert_eq!(layout.y_for(-90.0), layout.plot.bottom);
        assert_eq!(layout.y_for(-200.0), layout.plot.bottom);
        assert_eq!(layout.x_for(0.0), layout.plot.right);
        assert_eq!(layout.x_for(300.0), layout.plot.left);
    }

    #[test]
    fn rounded_corners_cut_only_the_corners() {
        assert!(outside_rounded_rect(0, 0, 100, 50, 8));
        assert!(!outside_rounded_rect(8, 0, 100, 50, 8));
        assert!(!outside_rounded_rect(50, 25, 100, 50, 8));
        assert!(outside_rounded_rect(99, 49, 100, 50, 8));
    }
}
