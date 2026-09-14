//! Window chrome.
//!
//! Windows draws the title bar itself, and by default it follows the system
//! theme — which leaves a light strip sitting above a dark Arciin sidebar. The
//! Desktop Window Manager lets an app choose those colours directly, so the
//! caption is painted the same near-black the Arciin shell uses and the window
//! reads as one surface from the top edge down.
//!
//! This is presentation only. Failing to apply it never blocks a window from
//! opening: on a build of Windows that does not support these attributes the
//! caption simply stays default.

/// `--background` / `--sidebar` from the reference app's `globals.css`.
pub const SHELL_R: u8 = 0x09;
pub const SHELL_G: u8 = 0x09;
pub const SHELL_B: u8 = 0x0b;

#[cfg(windows)]
mod imp {
    use super::*;

    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
    };

    /// `COLORREF` is `0x00BBGGRR`, not the RGB order the CSS token is written in.
    fn colorref(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32)
    }

    /// Set one DWM attribute, ignoring "this build doesn't know that attribute".
    ///
    /// # Safety
    /// `value` must point to a `size` byte value of the type the attribute
    /// expects, and `hwnd` must be a live top-level window.
    unsafe fn set_attribute<T>(hwnd: HWND, attribute: u32, value: &T) {
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attribute as i32),
                value as *const T as *const core::ffi::c_void,
                std::mem::size_of::<T>() as u32,
            )
        };
        if let Err(err) = result {
            // Expected on Windows 10, where caption colours are not settable.
            tracing::debug!(attribute, error = %err, "window caption attribute not applied");
        }
    }

    pub fn apply_dark_caption(hwnd: HWND) {
        let caption = colorref(SHELL_R, SHELL_G, SHELL_B);
        let text = colorref(0xff, 0xff, 0xff);
        // TRUE as a Win32 BOOL, so the minimise/maximise/close glyphs are
        // drawn light. Without it they stay dark and vanish into the caption.
        let dark: i32 = 1;

        // SAFETY: each value matches the size and type its attribute expects,
        // and the handle comes from a window Tauri has just created.
        unsafe {
            set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE.0 as u32, &dark);
            set_attribute(hwnd, DWMWA_CAPTION_COLOR.0 as u32, &caption);
            set_attribute(hwnd, DWMWA_TEXT_COLOR.0 as u32, &text);
            set_attribute(hwnd, DWMWA_BORDER_COLOR.0 as u32, &caption);
        }
    }
}

/// Paint a window's title bar to match the Arciin shell.
pub fn apply_dark_caption(window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        match window.hwnd() {
            Ok(hwnd) => imp::apply_dark_caption(hwnd),
            Err(err) => {
                tracing::debug!(error = %err, "no window handle; caption left default")
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}
