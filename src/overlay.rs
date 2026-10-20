//! Click-through, per-pixel-alpha topmost window that draws one line of text.

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{FW_SEMIBOLD, SetBkMode, SetTextColor, TRANSPARENT, TextOutW};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_TOPMOST, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    ShowWindow, WNDPROC, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT,
};
use windows::core::{PCWSTR, Result, w};

use crate::gdi::{self, Canvas, Font};
use crate::readouts::Segment;
use crate::taskbar::{Anchor, scale};

pub const WINDOW_CLASS: PCWSTR = w!("MicDbTaskbarOverlay");
/// Font height in 96-DPI pixels (matches the taskbar clock).
const FONT_PX_LOGICAL: i32 = 13;
const PADDING_LOGICAL: i32 = 4;

/// Opacity (0-255) of the contrast shadow drawn one pixel below-right of the text.
const SHADOW_ALPHA: u32 = 170;

type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Palette {
    text: Rgb,
    alarm: Rgb,
    shadow: Rgb,
}

const DARK_THEME: Palette = Palette {
    text: (0xFF, 0xFF, 0xFF),
    alarm: (0xFF, 0x45, 0x3A),
    shadow: (0x00, 0x00, 0x00),
};
const LIGHT_THEME: Palette = Palette {
    text: (0x1A, 0x1A, 0x1A),
    alarm: (0xD0, 0x10, 0x10),
    shadow: (0xFF, 0xFF, 0xFF),
};
/// Segments are rasterised as coverage masks in separate channels, then recoloured.
const NORMAL_MASK: COLORREF = COLORREF(0x0000_FF00);
const ALARM_MASK: COLORREF = COLORREF(0x0000_00FF);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    segments: Vec<Segment>,
    anchor: Anchor,
    palette: Palette,
}

pub struct Overlay {
    hwnd: HWND,
    last: Option<Frame>,
    rect: Option<RECT>,
    visible: bool,
}

impl Overlay {
    pub fn create(wndproc: WNDPROC) -> Result<Self> {
        let hwnd = gdi::create_window(
            WINDOW_CLASS,
            wndproc,
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST,
        )?;
        Ok(Self {
            hwnd,
            last: None,
            rect: None,
            visible: false,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Screen rectangle of the readout while it is visible.
    pub fn rect(&self) -> Option<RECT> {
        self.rect.filter(|_| self.visible)
    }

    /// Draws `segments` at `anchor`, or hides the window when `anchor` is `None`.
    pub fn show(&mut self, segments: &[Segment], anchor: Option<Anchor>) -> Result<()> {
        let Some(anchor) = anchor else {
            self.hide();
            return Ok(());
        };
        let palette = if gdi::is_light_theme() {
            LIGHT_THEME
        } else {
            DARK_THEME
        };
        let frame = Frame {
            segments: segments.to_vec(),
            anchor,
            palette,
        };
        if self.last.as_ref() != Some(&frame) {
            let size = self.render(&frame)?;
            self.rect = Some(RECT {
                left: anchor.left,
                top: anchor.top,
                right: anchor.left + size.cx,
                bottom: anchor.top + size.cy,
            });
            self.last = Some(frame);
        }
        if !self.visible {
            // SAFETY: valid window handle owned by this struct.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.visible = true;
        }
        Ok(())
    }

    /// Puts the window back on top of the taskbar, which reclaims z-order when clicked.
    pub fn reassert_topmost(&self) {
        if self.visible {
            reassert_topmost(self.hwnd);
        }
    }

    fn hide(&mut self) {
        if self.visible {
            // SAFETY: valid window handle.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
    }

    fn render(&self, frame: &Frame) -> Result<SIZE> {
        let dpi = frame.anchor.dpi;
        let font = Font::new(scale(FONT_PX_LOGICAL, dpi), FW_SEMIBOLD)?;
        let padding = scale(PADDING_LOGICAL, dpi);
        let runs: Vec<(Vec<u16>, bool)> = frame
            .segments
            .iter()
            .map(|segment| (gdi::wide(&segment.text), segment.alarm))
            .collect();
        let extents: Vec<SIZE> = {
            let probe = Canvas::new(SIZE { cx: 1, cy: 1 })?;
            font.select(probe.dc());
            runs.iter()
                .map(|(text, _)| gdi::text_extent(probe.dc(), text))
                .collect()
        };
        let text_width: i32 = extents.iter().map(|extent| extent.cx).sum();
        let text_height = extents.iter().map(|extent| extent.cy).max().unwrap_or(0);
        let mut canvas = Canvas::new(SIZE {
            cx: text_width + 2 * padding,
            cy: frame.anchor.height,
        })?;
        font.select(canvas.dc());
        let y = (canvas.size.cy - text_height) / 2;
        let mut x = padding;
        // SAFETY: drawing into our own memory DC.
        unsafe {
            SetBkMode(canvas.dc(), TRANSPARENT);
            for ((text, alarm), extent) in runs.iter().zip(&extents) {
                SetTextColor(canvas.dc(), if *alarm { ALARM_MASK } else { NORMAL_MASK });
                let _ = TextOutW(canvas.dc(), x, y, text);
                x += extent.cx;
            }
        }
        let width = canvas.size.cx as usize;
        compose(
            canvas.pixels(),
            width,
            frame.palette,
            scale(1, dpi).max(1) as usize,
        );
        canvas.present(
            self.hwnd,
            POINT {
                x: frame.anchor.left,
                y: frame.anchor.top,
            },
        )?;
        Ok(canvas.size)
    }
}

/// Re-inserts `hwnd` at the top of the topmost band without moving or activating it.
pub fn reassert_topmost(hwnd: HWND) {
    // SAFETY: plain z-order change on a window we own.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Converts the green (normal) and red (alarm) coverage masks into premultiplied BGRA in
/// the palette colours, with a soft offset shadow behind for contrast on translucent taskbars.
fn compose(pixels: &mut [u32], width: usize, palette: Palette, shadow_offset: usize) {
    let coverage: Vec<(u32, u32)> = pixels
        .iter()
        .map(|pixel| {
            let [_, green, red, _] = pixel.to_le_bytes();
            (u32::from(green), u32::from(red))
        })
        .collect();
    for (index, pixel) in pixels.iter_mut().enumerate() {
        let (x, y) = (index % width, index / width);
        let (normal, alarm) = coverage[index];
        let text = normal.max(alarm);
        let shadow = if x >= shadow_offset && y >= shadow_offset {
            let (n, a) = coverage[index - shadow_offset * width - shadow_offset];
            n.max(a) * SHADOW_ALPHA / 255
        } else {
            0
        };
        let shadow_visible = shadow * (255 - text) / 255;
        let channel = |n: u8, a: u8, s: u8| {
            let fill = (u32::from(n) * normal + u32::from(a) * alarm).min(255 * text);
            (fill + u32::from(s) * shadow_visible) / 255
        };
        let (nr, ng, nb) = palette.text;
        let (ar, ag, ab) = palette.alarm;
        let (sr, sg, sb) = palette.shadow;
        *pixel = ((text + shadow_visible) << 24)
            | (channel(nr, ar, sr) << 16)
            | (channel(ng, ag, sg) << 8)
            | channel(nb, ab, sb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_becomes_premultiplied_alpha() {
        // Green = normal coverage, red = alarm coverage.
        let mut pixels = [0x0000_8000u32, 0, 0x00FF_0000];
        let palette = Palette {
            text: (0xFF, 0x00, 0x80),
            alarm: (0x00, 0xFF, 0x00),
            shadow: (0, 0, 0),
        };
        compose(&mut pixels, 3, palette, 1);
        assert_eq!(pixels, [0x8080_0040, 0, 0xFF00_FF00]);
    }

    #[test]
    fn shadow_trails_text_diagonally() {
        let mut pixels = [0x0000_FF00u32, 0, 0, 0];
        compose(&mut pixels, 2, DARK_THEME, 1);
        assert_eq!(pixels[3] >> 24, SHADOW_ALPHA);
        assert_eq!(pixels[3] & 0x00FF_FFFF, 0);
    }
}
