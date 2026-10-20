//! Small RAII wrappers over GDI shared by the overlay and the popup chart.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreatePen, CreateSolidBrush, DEFAULT_CHARSET, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    FF_DONTCARE, FONT_WEIGHT, GetTextExtentPoint32W, HBITMAP, HDC, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, PEN_STYLE, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, RegisterClassW, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE, WNDCLASSW,
    WNDPROC, WS_POPUP,
};
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

const FONT_FACE: PCWSTR = w!("Segoe UI");

/// Registers `class` and creates a hidden popup window with the given extended styles.
pub fn create_window(class: PCWSTR, wndproc: WNDPROC, ex_style: WINDOW_EX_STYLE) -> Result<HWND> {
    // SAFETY: standard window class registration and creation on the UI thread.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_thread());
        }
        CreateWindowExW(
            ex_style,
            class,
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
        )
    }
}

/// A memory DC with a top-down 32-bit DIB selected into it.
pub struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    pub size: SIZE,
}

impl Canvas {
    pub fn new(size: SIZE) -> Result<Self> {
        let size = SIZE {
            cx: size.cx.max(1),
            cy: size.cy.max(1),
        };
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
        // SAFETY: the DIB is width*height 32-bit pixels at `bits`; all handles are owned here.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(Error::from_thread());
            }
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(bitmap) if !bits.is_null() => bitmap,
                other => {
                    if let Ok(bitmap) = other {
                        let _ = DeleteObject(HGDIOBJ(bitmap.0));
                    }
                    let _ = DeleteDC(dc);
                    return Err(Error::from_thread());
                }
            };
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            let mut canvas = Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
                size,
            };
            canvas.pixels().fill(0);
            Ok(canvas)
        }
    }

    pub fn dc(&self) -> HDC {
        self.dc
    }

    /// BGRA pixels, row-major, top row first.
    pub fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: `bits` points to exactly cx*cy pixels owned by `bitmap` for our lifetime;
        // GDI does not draw concurrently because all drawing happens on this thread.
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.size.cx * self.size.cy) as usize) }
    }

    /// Pushes the canvas (premultiplied BGRA) to a layered window at `origin`.
    pub fn present(&self, hwnd: HWND, origin: POINT) -> Result<()> {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
            ..Default::default()
        };
        // SAFETY: valid window and DC handles.
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&origin),
                Some(&self.size),
                Some(self.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: restores the original bitmap before deleting the handles we own.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

/// A font selected into a DC for the guard's lifetime.
pub struct Font(HFONT);

impl Font {
    pub fn new(height_px: i32, weight: FONT_WEIGHT) -> Result<Self> {
        // SAFETY: plain font creation.
        let font = unsafe {
            CreateFontW(
                -height_px,
                0,
                0,
                0,
                weight.0 as i32,
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

    pub fn select(&self, dc: HDC) {
        // SAFETY: valid handles; the font outlives every use of the DC by the caller.
        unsafe {
            SelectObject(dc, HGDIOBJ(self.0.0));
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

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Size of `text` in the font currently selected into `dc`.
pub fn text_extent(dc: HDC, text: &[u16]) -> SIZE {
    let mut extent = SIZE::default();
    // SAFETY: valid DC and text slice.
    unsafe {
        let _ = GetTextExtentPoint32W(dc, text, &mut extent);
    }
    extent
}

/// `COLORREF` from an `(r, g, b)` triple.
pub fn colorref((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
}

/// True when the taskbar uses the light theme (`SystemUsesLightTheme = 1`).
pub fn is_light_theme() -> bool {
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
    status.is_ok() && value == 1
}

/// An owned pen or brush selected into a DC; restores the previous object and deletes
/// itself on drop.
pub struct Tool {
    dc: HDC,
    object: HGDIOBJ,
    previous: HGDIOBJ,
}

impl Tool {
    pub fn pen(dc: HDC, style: PEN_STYLE, width: i32, color: COLORREF) -> Self {
        // SAFETY: creates and selects a pen we own.
        unsafe { Self::select(dc, HGDIOBJ(CreatePen(style, width, color).0)) }
    }

    pub fn brush(dc: HDC, color: COLORREF) -> Self {
        // SAFETY: creates and selects a brush we own.
        unsafe { Self::select(dc, HGDIOBJ(CreateSolidBrush(color).0)) }
    }

    unsafe fn select(dc: HDC, object: HGDIOBJ) -> Self {
        // SAFETY: caller passes a freshly created object.
        let previous = unsafe { SelectObject(dc, object) };
        Self {
            dc,
            object,
            previous,
        }
    }
}

impl Drop for Tool {
    fn drop(&mut self) {
        // SAFETY: deselect before deleting the object we own.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.object);
        }
    }
}
