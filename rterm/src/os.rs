//! The window's frame and backdrop on Windows: the same treatment ride's main window gets.

#[cfg(windows)]
mod imp {
    #[repr(C)]
    struct Margins {
        left: i32,
        right: i32,
        top: i32,
        bottom: i32,
    }

    type SubclassProc = unsafe extern "system" fn(isize, u32, usize, isize, usize, usize) -> isize;

    #[link(name = "user32")]
    extern "system" {
        fn IsZoomed(window: isize) -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
        fn SetWindowPos(
            window: isize,
            after: isize,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn GetProcAddress(module: isize, name: *const u8) -> *const std::ffi::c_void;
    }

    type SetComposition = unsafe extern "system" fn(isize, *mut CompositionData) -> i32;

    /// `SetWindowCompositionAttribute`, which user32 has but doesn't list for linking.
    fn set_composition() -> Option<SetComposition> {
        let user32: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        // SAFETY: user32 is loaded in every windowed process, the names end in a zero, and
        // the function found has the signature `SetComposition` describes.
        unsafe {
            let module = GetModuleHandleW(user32.as_ptr());
            let found = GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr().cast());
            (!found.is_null()).then(|| std::mem::transmute::<_, SetComposition>(found))
        }
    }

    /// What `SetWindowCompositionAttribute` takes to give a window a backdrop of its own.
    #[repr(C)]
    struct AccentPolicy {
        state: i32,
        flags: i32,
        /// The tint laid over the blur, as 0xAABBGGRR.
        tint: u32,
        animation: i32,
    }

    #[repr(C)]
    struct CompositionData {
        attribute: u32,
        data: *mut AccentPolicy,
        size: usize,
    }

    #[link(name = "comctl32")]
    extern "system" {
        fn SetWindowSubclass(window: isize, proc: SubclassProc, id: usize, data: usize) -> i32;
        fn DefSubclassProc(window: isize, message: u32, wparam: usize, lparam: isize) -> isize;
    }

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmExtendFrameIntoClientArea(window: isize, margins: *const Margins) -> i32;
        fn DwmSetWindowAttribute(
            window: isize,
            attribute: u32,
            value: *const i32,
            size: u32,
        ) -> i32;
    }

    fn hwnd(window: &impl raw_window_handle::HasWindowHandle) -> Option<isize> {
        match window.window_handle().ok()?.as_raw() {
            raw_window_handle::RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
            _ => None,
        }
    }

    /// Removes the system title bar and borders from view while the window keeps them in
    /// its style. Windows then still treats it as a normal framed window: rounded corners,
    /// a shadow, snapping, and (what this is for) a blurred backdrop, which it only draws
    /// for framed windows.
    unsafe extern "system" fn hide_frame(
        window: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        _data: usize,
    ) -> isize {
        const WM_NCCALCSIZE: u32 = 0x0083;
        const SM_CXSIZEFRAME: i32 = 32;
        const SM_CXPADDEDBORDER: i32 = 92;
        if message == WM_NCCALCSIZE && wparam != 0 {
            // Answering 0 makes the client area cover the whole window. A maximized window
            // hangs over the screen edge by its frame's thickness, so that much is taken off.
            if IsZoomed(window) != 0 {
                let frame = GetSystemMetrics(SM_CXSIZEFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER);
                // SAFETY: for this message, lparam points at NCCALCSIZE_PARAMS, which starts with the window's RECT.
                let rect = &mut *(lparam as *mut [i32; 4]);
                rect[0] += frame;
                rect[1] += frame;
                rect[2] -= frame;
                rect[3] -= frame;
            }
            return 0;
        }
        DefSubclassProc(window, message, wparam, lparam)
    }

    /// Takes over the window's frame (see `hide_frame`). Call once, on the window's own thread.
    pub fn adopt_window(window: &impl raw_window_handle::HasWindowHandle) -> bool {
        const SWP_FRAME_ONLY: u32 = 0x0020 | 0x0002 | 0x0001 | 0x0004 | 0x0010;
        let Some(window) = hwnd(window) else {
            return false;
        };
        // SAFETY: the handle is this process's live window, and the calls keep no pointers.
        unsafe {
            if SetWindowSubclass(window, hide_frame, 1, 0) == 0 {
                return false;
            }
            // No part of the client area counts as frame: where it did, the system drew its
            // own caption buttons under the see-through title bar. The blur doesn't need it.
            let margins = Margins {
                left: 0,
                right: 0,
                top: 0,
                bottom: 0,
            };
            DwmExtendFrameIntoClientArea(window, &margins);
            // Makes Windows recompute the frame now, through `hide_frame`.
            SetWindowPos(window, 0, 0, 0, 0, 0, SWP_FRAME_ONLY);
        }
        true
    }

    /// Puts a blurred view of what is behind the window under it, tinted with `tint`
    /// (red, green, blue, alpha), or takes it away when there is no tint. `dark` is for the
    /// system's own parts of the window.
    ///
    /// The blur is the window's own accent rather than the system's acrylic backdrop: that
    /// one is a pale, thin material that turns milky over anything bright, and goes flat
    /// whenever the window isn't the active one. This one keeps the theme's colour under
    /// the panes and stays blurred.
    ///
    /// It is the plain blur accent, not the acrylic one: with acrylic, Windows moved and
    /// resized the window in slow motion, and the whole window lagged after a while.
    pub fn set_backdrop(
        window: &impl raw_window_handle::HasWindowHandle,
        tint: Option<[u8; 4]>,
        dark: bool,
    ) -> bool {
        const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
        const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
        const BACKDROP_NONE: i32 = 1;
        const BACKDROP_ACRYLIC: i32 = 3;
        const WCA_ACCENT_POLICY: u32 = 19;
        const ACCENT_DISABLED: i32 = 0;
        const ACCENT_BLUR: i32 = 3;
        // Tells the accent to use the tint given.
        const ACCENT_TINTED: i32 = 2;
        let Some(window) = hwnd(window) else {
            return false;
        };
        let set = |attribute: u32, value: i32| {
            // SAFETY: the value outlives the call and its size is given.
            unsafe { DwmSetWindowAttribute(window, attribute, &value, 4) == 0 }
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, dark as i32);
        let mut accent = AccentPolicy {
            state: if tint.is_some() {
                ACCENT_BLUR
            } else {
                ACCENT_DISABLED
            },
            flags: ACCENT_TINTED,
            tint: tint.map_or(0, |[r, g, b, a]| u32::from_le_bytes([r, g, b, a])),
            animation: 0,
        };
        let mut data = CompositionData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut accent,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        // SAFETY: both structs outlive the call, and the size given is the accent's own.
        let accented = set_composition().is_some_and(|set| unsafe { set(window, &mut data) != 0 });
        // Where the accent isn't there, the system's acrylic is better than nothing.
        let fallback = tint.is_some() && !accented;
        set(
            DWMWA_SYSTEMBACKDROP_TYPE,
            if fallback {
                BACKDROP_ACRYLIC
            } else {
                BACKDROP_NONE
            },
        ) || accented
    }
}

#[cfg(windows)]
pub use imp::{adopt_window, set_backdrop};

#[cfg(not(windows))]
pub fn adopt_window(_window: &impl raw_window_handle::HasWindowHandle) -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_backdrop(
    _window: &impl raw_window_handle::HasWindowHandle,
    _tint: Option<[u8; 4]>,
    _dark: bool,
) -> bool {
    false
}
