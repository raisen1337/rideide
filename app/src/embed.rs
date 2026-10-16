//! Another app's window shown inside ride's: the Designer holds the Tribib app's own window
//! in its middle, so the real editor (its projects, its tools) is used right there.
//!
//! The window becomes a child of ride's, without its frame, and is put back as it was when
//! ride closes, or lets it go. A child window goes when its parent does, so this matters:
//! ride gives it back on every way out it has (closing, a failed frame, a crash it notices).

#[cfg(windows)]
mod imp {
    use std::sync::Mutex;

    #[repr(C)]
    #[derive(Default, Clone, Copy, Debug, PartialEq)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    type EnumProc = unsafe extern "system" fn(isize, isize) -> i32;

    /// WINDOWPLACEMENT: where a window goes when it isn't minimized or maximized.
    #[repr(C)]
    #[derive(Default)]
    struct Placement {
        length: u32,
        flags: u32,
        show: u32,
        min: [i32; 2],
        max: [i32; 2],
        normal: Rect,
    }

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(proc: EnumProc, data: isize) -> i32;
        fn GetWindowThreadProcessId(window: isize, pid: *mut u32) -> u32;
        fn IsWindowVisible(window: isize) -> i32;
        fn IsWindow(window: isize) -> i32;
        fn IsZoomed(window: isize) -> i32;
        fn IsIconic(window: isize) -> i32;
        fn GetWindowRect(window: isize, rect: *mut Rect) -> i32;
        fn GetWindowLongPtrW(window: isize, index: i32) -> isize;
        fn SetWindowLongPtrW(window: isize, index: i32, value: isize) -> isize;
        fn SetParent(child: isize, parent: isize) -> isize;
        fn GetParent(window: isize) -> isize;
        fn GetWindow(window: isize, command: u32) -> isize;
        fn ShowWindow(window: isize, command: i32) -> i32;
        fn SetWindowPos(
            window: isize,
            after: isize,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            flags: u32,
        ) -> i32;
        fn GetClassNameW(window: isize, name: *mut u16, size: i32) -> i32;
        fn GetWindowTextLengthW(window: isize) -> i32;
        fn GetWindowPlacement(window: isize, placement: *mut Placement) -> i32;
        fn GetClientRect(window: isize, rect: *mut Rect) -> i32;
        fn ClientToScreen(window: isize, point: *mut [i32; 2]) -> i32;
        fn SetWindowRgn(window: isize, region: isize, redraw: i32) -> i32;
        fn PostMessageW(window: isize, message: u32, wparam: usize, lparam: isize) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateRectRgn(left: i32, top: i32, right: i32, bottom: i32) -> isize;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn CloseHandle(handle: isize) -> i32;
        fn QueryFullProcessImageNameW(
            process: isize,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
    }

    const GWL_STYLE: i32 = -16;
    const GWL_EXSTYLE: i32 = -20;
    const WS_CHILD: isize = 0x4000_0000;
    const WS_POPUP: isize = 0x8000_0000u32 as i32 as isize;
    const WS_CAPTION: isize = 0x00C0_0000;
    const WS_THICKFRAME: isize = 0x0004_0000;
    const WS_SYSMENU: isize = 0x0008_0000;
    const WS_MINIMIZEBOX: isize = 0x0002_0000;
    const WS_MAXIMIZEBOX: isize = 0x0001_0000;
    const WS_EX_APPWINDOW: isize = 0x0004_0000;
    const SW_HIDE: i32 = 0;
    const SW_SHOW: i32 = 5;
    const SW_RESTORE: i32 = 9;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    const SWP_SHOWWINDOW: u32 = 0x0040;
    const GW_OWNER: u32 = 4;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    /// A window held inside ride's, and how it was before.
    #[derive(Clone, Copy, Debug)]
    pub struct Held {
        pub window: isize,
        style: isize,
        exstyle: isize,
        rect: Rect,
        maximized: bool,
    }

    /// What is held, for giving it back on the way out (the panic hook reads it).
    static HELD: Mutex<Option<Held>> = Mutex::new(None);

    /// The program a window belongs to: its executable's file name, lower case.
    fn program_of(window: isize) -> Option<String> {
        let mut pid = 0u32;
        // SAFETY: the calls write only into the buffers given, with their sizes.
        unsafe {
            GetWindowThreadProcessId(window, &mut pid);
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process == 0 {
                return None;
            }
            let mut buffer = [0u16; 520];
            let mut size = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size);
            CloseHandle(process);
            if ok == 0 {
                return None;
            }
            let path = String::from_utf16_lossy(&buffer[..size as usize]);
            Some(path.rsplit('\\').next().unwrap_or(&path).to_lowercase())
        }
    }

    fn class_of(window: isize) -> String {
        let mut buffer = [0u16; 128];
        // SAFETY: writes at most the given number of chars into the buffer.
        let n = unsafe { GetClassNameW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
        String::from_utf16_lossy(&buffer[..n.max(0) as usize])
    }

    unsafe extern "system" fn collect(window: isize, data: isize) -> i32 {
        // SAFETY: `data` is the Vec `find` passes in, alive for the whole enumeration.
        let list = &mut *(data as *mut Vec<isize>);
        list.push(window);
        1
    }

    /// The main window of a running program (by executable name, e.g. "tribib.exe"): of
    /// its visible top-level windows without an owner, the largest one that is a real
    /// window. Overlays that let clicks through or never take the keyboard (an app may keep
    /// one over the screen) are left out, and a minimized window counts by its real size.
    pub fn find(program: &str) -> Option<isize> {
        const WS_EX_TRANSPARENT: isize = 0x20;
        const WS_EX_TOOLWINDOW: isize = 0x80;
        const WS_EX_NOACTIVATE: isize = 0x0800_0000;
        let mut all: Vec<isize> = Vec::new();
        // SAFETY: the callback only pushes into the Vec, which outlives the call.
        unsafe { EnumWindows(collect, &mut all as *mut Vec<isize> as isize) };
        let program = program.to_lowercase();
        all.into_iter()
            .filter(|&w| unsafe { IsWindowVisible(w) != 0 && GetWindow(w, GW_OWNER) == 0 })
            .filter(|&w| {
                let ex = unsafe { GetWindowLongPtrW(w, GWL_EXSTYLE) };
                ex & (WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) == 0
            })
            .filter(|&w| program_of(w).as_deref() == Some(program.as_str()))
            .filter(|&w| !class_of(w).contains("Tooltip"))
            .max_by_key(|&w| {
                let mut placement = Placement {
                    length: std::mem::size_of::<Placement>() as u32,
                    ..Placement::default()
                };
                // SAFETY: the struct is sized as the call expects and only written to.
                let r = unsafe {
                    if GetWindowPlacement(w, &mut placement) != 0 {
                        placement.normal
                    } else {
                        let mut rect = Rect::default();
                        GetWindowRect(w, &mut rect);
                        rect
                    }
                };
                let titled = unsafe { GetWindowTextLengthW(w) } > 0;
                (
                    titled,
                    (r.right - r.left) as i64 * (r.bottom - r.top) as i64,
                )
            })
    }

    /// Takes a window into `parent`, without its frame.
    pub fn hold(window: isize, parent: isize) -> Option<Held> {
        // SAFETY: both handles are checked to be windows; the calls keep no pointers.
        unsafe {
            if IsWindow(window) == 0 || IsWindow(parent) == 0 {
                return None;
            }
            let maximized = IsZoomed(window) != 0;
            if maximized || IsIconic(window) != 0 {
                ShowWindow(window, SW_RESTORE);
            }
            let mut rect = Rect::default();
            GetWindowRect(window, &mut rect);
            let mut style = GetWindowLongPtrW(window, GWL_STYLE);
            let exstyle = GetWindowLongPtrW(window, GWL_EXSTYLE);
            // A window left as a child without a parent (by an earlier ride that didn't get
            // to give it back) goes back as a normal window, with its frame.
            if style & WS_CHILD != 0 {
                const WS_OVERLAPPEDWINDOW: isize = 0x00CF_0000;
                style = (style & !WS_CHILD) | WS_OVERLAPPEDWINDOW;
            }
            let held = Held {
                window,
                style,
                exstyle,
                rect,
                maximized,
            };
            *HELD.lock().unwrap() = Some(held);
            let frame = WS_POPUP
                | WS_CAPTION
                | WS_THICKFRAME
                | WS_SYSMENU
                | WS_MINIMIZEBOX
                | WS_MAXIMIZEBOX;
            SetWindowLongPtrW(window, GWL_STYLE, (style & !frame) | WS_CHILD);
            SetWindowLongPtrW(window, GWL_EXSTYLE, exstyle & !WS_EX_APPWINDOW);
            // Hidden until it is put where it goes: it would show at its old place meanwhile.
            ShowWindow(window, SW_HIDE);
            SetParent(window, parent);
            SetWindowPos(
                window,
                0,
                0,
                0,
                0,
                0,
                SWP_NOZORDER | SWP_FRAMECHANGED | 0x0001 | 0x0002,
            );
            Some(held)
        }
    }

    /// How far in from its window's edges the part a window draws begins, on each side.
    /// A frameless app keeps an invisible border it doesn't draw, even as a child.
    fn insets(window: isize) -> Rect {
        // SAFETY: the calls write only into the structs given.
        unsafe {
            let (mut outer, mut client) = (Rect::default(), Rect::default());
            GetWindowRect(window, &mut outer);
            GetClientRect(window, &mut client);
            let mut corner = [0i32, 0];
            ClientToScreen(window, &mut corner);
            Rect {
                left: corner[0] - outer.left,
                top: corner[1] - outer.top,
                right: outer.right - (corner[0] + client.right),
                bottom: outer.bottom - (corner[1] + client.bottom),
            }
        }
    }

    /// Where the held window is, in its parent's client pixels; hidden with `show` false.
    /// What it draws fills the place exactly: its undrawn border lies outside, cut off.
    pub fn place(held: &Held, x: i32, y: i32, w: i32, h: i32, show: bool) {
        // SAFETY: the calls keep no pointers; the region is handed over to the system.
        unsafe {
            if !show {
                // Hidden, and nothing of it could show even if the app showed itself again
                // (an app may, on its own events): no area to draw in, at one pixel's size.
                // Hiding alone left a white patch where it had been.
                const SWP_HIDEWINDOW: u32 = 0x0080;
                SetWindowRgn(held.window, CreateRectRgn(0, 0, 0, 0), 0);
                SetWindowPos(
                    held.window,
                    0,
                    0,
                    0,
                    1,
                    1,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_HIDEWINDOW,
                );
                ShowWindow(held.window, SW_HIDE);
                return;
            }
            // Its whole area again, before it is sized (a hidden one had none).
            SetWindowRgn(held.window, 0, 0);
            let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW;
            SetWindowPos(held.window, 0, x, y, w.max(1), h.max(1), flags);
            let edge = insets(held.window);
            if edge != Rect::default()
                && edge.left.max(edge.top).max(edge.right).max(edge.bottom) < 40
            {
                let (w2, h2) = (
                    w.max(1) + edge.left + edge.right,
                    h.max(1) + edge.top + edge.bottom,
                );
                SetWindowPos(held.window, 0, x - edge.left, y - edge.top, w2, h2, flags);
                // Only the drawn part shows; the border would lie over ride's panels.
                let region = CreateRectRgn(edge.left, edge.top, w2 - edge.right, h2 - edge.bottom);
                if region != 0 {
                    SetWindowRgn(held.window, region, 1);
                }
            }
        }
    }

    /// Whether the held window is still there and still inside ride's.
    pub fn alive(held: &Held, parent: isize) -> bool {
        // SAFETY: plain queries on handles.
        unsafe { IsWindow(held.window) != 0 && GetParent(held.window) == parent }
    }

    /// Whether the held window still exists at all.
    pub fn exists(held: &Held) -> bool {
        // SAFETY: a plain query on a handle.
        unsafe { IsWindow(held.window) != 0 }
    }

    /// Takes back a held window that got out of ride's (the app re-made or re-parented it
    /// itself): a child again, without its frame, hidden until it is placed. Without this it
    /// floated frameless over everything until closed.
    pub fn readopt(held: &Held, parent: isize) {
        let frame =
            WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        // SAFETY: the calls keep no pointers; a window gone meanwhile makes them do nothing.
        unsafe {
            if IsWindow(held.window) == 0 || IsWindow(parent) == 0 {
                return;
            }
            ShowWindow(held.window, SW_HIDE);
            let style = GetWindowLongPtrW(held.window, GWL_STYLE);
            SetWindowLongPtrW(held.window, GWL_STYLE, (style & !frame) | WS_CHILD);
            let ex = GetWindowLongPtrW(held.window, GWL_EXSTYLE);
            SetWindowLongPtrW(held.window, GWL_EXSTYLE, ex & !WS_EX_APPWINDOW);
            SetParent(held.window, parent);
            SetWindowPos(
                held.window,
                0,
                0,
                0,
                0,
                0,
                SWP_NOZORDER | SWP_FRAMECHANGED | 0x0001 | 0x0002,
            );
        }
    }

    /// Gives the window back as it was: its own window again, where it was, with its frame.
    pub fn release(held: &Held) {
        // SAFETY: the calls keep no pointers; a window gone meanwhile makes them do nothing.
        unsafe {
            if IsWindow(held.window) == 0 {
                *HELD.lock().unwrap() = None;
                return;
            }
            SetWindowRgn(held.window, 0, 1);
            SetParent(held.window, 0);
            SetWindowLongPtrW(held.window, GWL_STYLE, held.style);
            SetWindowLongPtrW(held.window, GWL_EXSTYLE, held.exstyle);
            let r = held.rect;
            SetWindowPos(
                held.window,
                0,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
            ShowWindow(held.window, if held.maximized { 3 } else { SW_SHOW });
        }
        *HELD.lock().unwrap() = None;
    }

    /// Closes a window ride started the app of, as its own close button would: given back
    /// first, still hidden, so it can close the way it always does without being seen.
    pub fn close(held: &Held) {
        const WM_CLOSE: u32 = 0x0010;
        // SAFETY: the calls keep no pointers; a window gone meanwhile makes them do nothing.
        unsafe {
            if IsWindow(held.window) != 0 {
                ShowWindow(held.window, SW_HIDE);
                SetWindowRgn(held.window, 0, 0);
                SetParent(held.window, 0);
                SetWindowLongPtrW(held.window, GWL_STYLE, held.style & !0x1000_0000);
                SetWindowLongPtrW(held.window, GWL_EXSTYLE, held.exstyle);
                PostMessageW(held.window, WM_CLOSE, 0, 0);
            }
        }
        *HELD.lock().unwrap() = None;
    }

    /// Gives back whatever is held: for the panic hook and the way out.
    pub fn release_any() {
        let held = HELD.lock().ok().and_then(|h| *h);
        if let Some(held) = held {
            release(&held);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    #[derive(Clone, Copy, Debug)]
    pub struct Held {
        pub window: isize,
    }
    pub fn find(_program: &str) -> Option<isize> {
        None
    }
    pub fn hold(_window: isize, _parent: isize) -> Option<Held> {
        None
    }
    pub fn place(_held: &Held, _x: i32, _y: i32, _w: i32, _h: i32, _show: bool) {}
    pub fn exists(_held: &Held) -> bool {
        false
    }
    pub fn readopt(_held: &Held, _parent: isize) {}
    pub fn alive(_held: &Held, _parent: isize) -> bool {
        false
    }
    pub fn release(_held: &Held) {}
    pub fn close(_held: &Held) {}
    pub fn release_any() {}
}

pub use imp::*;
