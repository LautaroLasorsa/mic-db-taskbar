//! Click-through, per-pixel-alpha topmost window that draws one line of text.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject, FF_DONTCARE, FW_SEMIBOLD,
    GetTextExtentPoint32W, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HWND_TOPMOST, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WNDCLASSW,
    WNDPROC, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

use crate::taskbar::{Anchor, scale};

pub const WINDOW_CLASS: PCWSTR = w!("MicDbTaskbarOverlay");
const FONT_FACE: PCWSTR = w!("Segoe UI");
/// Font height in 96-DPI pixels (matches the taskbar clock).
const FONT_PX_LOGICAL: i32 = 13;
const PADDING_LOGICAL: i32 = 4;
/// Opacity (0-255) of the contrast shadow drawn one pixel below-right of the text.
const SHADOW_ALPHA: u32 = 170;

type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Palette {
    text: Rgb,
    shadow: Rgb,
}

const DARK_THEME: Palette = Palette {
    text: (0xFF, 0xFF, 0xFF),
    shadow: (0x00, 0x00, 0x00),
};
const LIGHT_THEME: Palette = Palette {
    text: (0x1A, 0x1A, 0x1A),
    shadow: (0xFF, 0xFF, 0xFF),
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    text: String,
    anchor: Anchor,
    palette: Palette,
}

pub struct Overlay {
    hwnd: HWND,
    last: Option<Frame>,
    visible: bool,
}

impl Overlay {
    pub fn create(wndproc: WNDPROC) -> Result<Self> {
        // SAFETY: standard window class registration and creation on the UI thread.
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: wndproc,
                hInstance: instance.into(),
                lpszClassName: WINDOW_CLASS,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(Error::from_thread());
            }
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_LAYERED
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST,
                WINDOW_CLASS,
                w!("mic-db-taskbar"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            Ok(Self {
                hwnd,
                last: None,
                visible: false,
            })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Draws `text` at `anchor`, or hides the window when `anchor` is `None`.
    pub fn show(&mut self, text: &str, anchor: Option<Anchor>) -> Result<()> {
        let Some(anchor) = anchor else {
            self.hide();
            return Ok(());
        };
        let frame = Frame {
            text: text.to_owned(),
            anchor,
            palette: theme_palette(),
        };
        if self.last.as_ref() != Some(&frame) {
            self.render(&frame)?;
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
        if !self.visible {
            return;
        }
        // SAFETY: valid window handle.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
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

    fn render(&self, frame: &Frame) -> Result<()> {
        let dc = MemoryDc::new()?;
        let font = Font::new(scale(FONT_PX_LOGICAL, frame.anchor.dpi))?;
        let wide: Vec<u16> = frame.text.encode_utf16().collect();
        let padding = scale(PADDING_LOGICAL, frame.anchor.dpi);
        // SAFETY: GDI drawing into a DIB we own; the pixel slice covers exactly the DIB.
        unsafe {
            let previous_font = SelectObject(dc.0, HGDIOBJ(font.0.0));
            let mut extent = SIZE::default();
            let _ = GetTextExtentPoint32W(dc.0, &wide, &mut extent);
            let size = SIZE {
                cx: extent.cx + 2 * padding,
                cy: frame.anchor.height.max(1),
            };
            let (bitmap, pixels) = create_dib(dc.0, size)?;
            let previous_bitmap = SelectObject(dc.0, HGDIOBJ(bitmap.0));
            pixels.fill(0);
            SetBkMode(dc.0, TRANSPARENT);
            SetTextColor(dc.0, COLORREF(0x00FF_FFFF));
            let _ = TextOutW(dc.0, padding, (size.cy - extent.cy) / 2, &wide);
            compose(
                pixels,
                size.cx as usize,
                frame.palette,
                scale(1, frame.anchor.dpi).max(1) as usize,
            );

            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
                ..Default::default()
            };
            let result = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&POINT {
                    x: frame.anchor.left,
                    y: frame.anchor.top,
                }),
                Some(&size),
                Some(dc.0),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            SelectObject(dc.0, previous_bitmap);
            SelectObject(dc.0, previous_font);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            result
        }
    }
}

/// Converts white-on-black grayscale text into premultiplied BGRA in the palette's text
/// colour, with a soft offset shadow behind it for contrast on translucent taskbars.
fn compose(pixels: &mut [u32], width: usize, palette: Palette, shadow_offset: usize) {
    let coverage: Vec<u32> = pixels
        .iter()
        .map(|pixel| {
            let [blue, green, red, _] = pixel.to_le_bytes();
            u32::from(red.max(green).max(blue))
        })
        .collect();
    for (index, pixel) in pixels.iter_mut().enumerate() {
        let (x, y) = (index % width, index / width);
        let text = coverage[index];
        let shadow = if x >= shadow_offset && y >= shadow_offset {
            coverage[index - shadow_offset * width - shadow_offset] * SHADOW_ALPHA / 255
        } else {
            0
        };
        let shadow_visible = shadow * (255 - text) / 255;
        let channel = |t: u8, s: u8| (u32::from(t) * text + u32::from(s) * shadow_visible) / 255;
        let (tr, tg, tb) = palette.text;
        let (sr, sg, sb) = palette.shadow;
        *pixel = ((text + shadow_visible) << 24)
            | (channel(tr, sr) << 16)
            | (channel(tg, sg) << 8)
            | channel(tb, sb);
    }
}

struct MemoryDc(HDC);

impl MemoryDc {
    fn new() -> Result<Self> {
        // SAFETY: a memory DC compatible with the screen.
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            Err(Error::from_thread())
        } else {
            Ok(Self(dc))
        }
    }
}

impl Drop for MemoryDc {
    fn drop(&mut self) {
        // SAFETY: DC created by CreateCompatibleDC and owned here.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct Font(HFONT);

impl Font {
    fn new(height_px: i32) -> Result<Self> {
        // SAFETY: plain font creation.
        let font = unsafe {
            CreateFontW(
                -height_px,
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                FF_DONTCARE.0 as u32,
                FONT_FACE,
            )
        };
        if font.is_invalid() {
            Err(Error::from_thread())
        } else {
            Ok(Self(font))
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: font created by CreateFontW and owned here.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}

/// Creates a top-down 32-bit DIB and returns it with a mutable view of its pixels.
unsafe fn create_dib<'a>(
    dc: HDC,
    size: SIZE,
) -> Result<(windows::Win32::Graphics::Gdi::HBITMAP, &'a mut [u32])> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size.cx,
            biHeight: -size.cy,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: CreateDIBSection allocates width*height 32-bit pixels at `bits`.
    unsafe {
        let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            return Err(Error::from_thread());
        }
        let len = (size.cx * size.cy) as usize;
        Ok((
            bitmap,
            std::slice::from_raw_parts_mut(bits.cast::<u32>(), len),
        ))
    }
}

/// Palette matching the taskbar theme (`SystemUsesLightTheme`).
fn theme_palette() -> Palette {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reads one DWORD into a correctly sized buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut value).cast()),
            Some(&mut size),
        )
    };
    if status.is_ok() && value == 1 {
        LIGHT_THEME
    } else {
        DARK_THEME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_becomes_premultiplied_alpha() {
        let mut pixels = [0x0080_8080u32, 0, 0x00FF_FFFF];
        let palette = Palette {
            text: (0xFF, 0x00, 0x80),
            shadow: (0, 0, 0),
        };
        compose(&mut pixels, 3, palette, 1);
        assert_eq!(pixels, [0x8080_0040, 0, 0xFFFF_0080]);
    }

    #[test]
    fn shadow_trails_text_diagonally() {
        let mut pixels = [0x00FF_FFFFu32, 0, 0, 0];
        compose(&mut pixels, 2, DARK_THEME, 1);
        assert_eq!(pixels[3] >> 24, SHADOW_ALPHA);
        assert_eq!(pixels[3] & 0x00FF_FFFF, 0);
    }
}
