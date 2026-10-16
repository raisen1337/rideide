//! What rdrop asks of Windows itself: the user's folders where they really are, the drives
//! with their names and free space, the Recycle Bin, and the clipboard's list of files,
//! which Explorer shares, so copy and paste work between the two.

use std::path::PathBuf;

/// One of the user's folders, as Explorer's Quick access shows it.
pub struct Known {
    pub name: &'static str,
    pub path: PathBuf,
}

#[derive(Clone)]
pub struct Drive {
    /// "C:\"
    pub root: String,
    pub label: String,
    pub kind: DriveKind,
    pub free: u64,
    pub total: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Other,
}

impl Drive {
    /// "Local Disk (C:)", as Explorer names it.
    pub fn title(&self) -> String {
        let letter = self.root.trim_end_matches('\\');
        let name = if self.label.is_empty() {
            match self.kind {
                DriveKind::Fixed => "Local Disk",
                DriveKind::Removable => "USB Drive",
                DriveKind::Network => "Network Drive",
                DriveKind::Optical => "DVD Drive",
                DriveKind::Other => "Drive",
            }
        } else {
            &self.label
        };
        format!("{name} ({letter})")
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(windows)]
mod ffi {
    #[repr(C)]
    pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);

    #[link(name = "shell32")]
    extern "system" {
        pub fn SHGetKnownFolderPath(
            id: *const Guid,
            flags: u32,
            token: isize,
            path: *mut *mut u16,
        ) -> i32;
        pub fn SHFileOperationW(op: *mut FileOp) -> i32;
        pub fn DragQueryFileW(drop: isize, index: u32, file: *mut u16, len: u32) -> u32;
    }

    #[link(name = "ole32")]
    extern "system" {
        pub fn CoTaskMemFree(p: *mut std::ffi::c_void);
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetLogicalDrives() -> u32;
        pub fn GetDriveTypeW(root: *const u16) -> u32;
        pub fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            max_component: *mut u32,
            flags: *mut u32,
            fs: *mut u16,
            fs_len: u32,
        ) -> i32;
        pub fn GetDiskFreeSpaceExW(
            root: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
        pub fn GlobalAlloc(flags: u32, bytes: usize) -> isize;
        pub fn GlobalLock(mem: isize) -> *mut u8;
        pub fn GlobalUnlock(mem: isize) -> i32;
        pub fn GlobalFree(mem: isize) -> isize;
        pub fn SetErrorMode(mode: u32) -> u32;
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn OpenClipboard(owner: isize) -> i32;
        pub fn CloseClipboard() -> i32;
        pub fn EmptyClipboard() -> i32;
        pub fn GetClipboardData(format: u32) -> isize;
        pub fn SetClipboardData(format: u32, mem: isize) -> isize;
        pub fn IsClipboardFormatAvailable(format: u32) -> i32;
        pub fn RegisterClipboardFormatW(name: *const u16) -> u32;
        pub fn GetClipboardSequenceNumber() -> u32;
    }

    /// SHFILEOPSTRUCTW.
    #[repr(C)]
    pub struct FileOp {
        pub window: isize,
        pub func: u32,
        pub from: *const u16,
        pub to: *const u16,
        pub flags: u16,
        pub aborted: i32,
        pub mappings: *mut std::ffi::c_void,
        pub title: *const u16,
    }

    pub const CF_HDROP: u32 = 15;
}

/// The user's folders, where Windows keeps them (OneDrive may have moved some).
pub fn known_folders() -> Vec<Known> {
    #[cfg(windows)]
    {
        use ffi::*;
        let list: [(&str, Guid); 6] = [
            (
                "Desktop",
                Guid(
                    0xB4BF_CC3A,
                    0xDB2C,
                    0x424C,
                    [0xB0, 0x29, 0x7F, 0xE9, 0x9A, 0x87, 0xC6, 0x41],
                ),
            ),
            (
                "Downloads",
                Guid(
                    0x374D_E290,
                    0x123F,
                    0x4565,
                    [0x91, 0x64, 0x39, 0xC4, 0x92, 0x5E, 0x46, 0x7B],
                ),
            ),
            (
                "Documents",
                Guid(
                    0xFDD3_9AD0,
                    0x238F,
                    0x46AF,
                    [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7],
                ),
            ),
            (
                "Pictures",
                Guid(
                    0x33E2_8130,
                    0x4E1E,
                    0x4676,
                    [0x83, 0x5A, 0x98, 0x39, 0x5C, 0x3B, 0xC3, 0xBB],
                ),
            ),
            (
                "Music",
                Guid(
                    0x4BD8_D571,
                    0x6D19,
                    0x48D3,
                    [0xBE, 0x97, 0x42, 0x22, 0x20, 0x08, 0x0E, 0x43],
                ),
            ),
            (
                "Videos",
                Guid(
                    0x1898_9B1D,
                    0x99B5,
                    0x455B,
                    [0x84, 0x1C, 0xAB, 0x7C, 0x74, 0xE4, 0xDD, 0xFC],
                ),
            ),
        ];
        list.into_iter()
            .filter_map(|(name, id)| {
                let mut out: *mut u16 = std::ptr::null_mut();
                // SAFETY: the GUID outlives the call; the returned string is copied and freed.
                unsafe {
                    if SHGetKnownFolderPath(&id, 0, 0, &mut out) != 0 || out.is_null() {
                        return None;
                    }
                    let len = (0..).take_while(|&i| *out.add(i) != 0).count();
                    let path = String::from_utf16_lossy(std::slice::from_raw_parts(out, len));
                    CoTaskMemFree(out.cast());
                    Some(Known {
                        name,
                        path: PathBuf::from(path),
                    })
                }
            })
            .filter(|k| k.path.is_dir())
            .collect()
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// The drives there are, with their names and how full they are.
pub fn drives() -> Vec<Drive> {
    #[cfg(windows)]
    {
        use ffi::*;
        // An empty card reader or DVD drive would otherwise put up a "no disk" box.
        // SAFETY: plain calls with valid buffers of the lengths given.
        unsafe {
            let old = SetErrorMode(0x0001);
            let mask = GetLogicalDrives();
            let mut drives = Vec::new();
            for i in 0..26u32 {
                if mask & (1 << i) == 0 {
                    continue;
                }
                let root = format!("{}:\\", (b'A' + i as u8) as char);
                let w = wide(&root);
                let kind = match GetDriveTypeW(w.as_ptr()) {
                    2 => DriveKind::Removable,
                    3 => DriveKind::Fixed,
                    4 => DriveKind::Network,
                    5 => DriveKind::Optical,
                    _ => DriveKind::Other,
                };
                let mut name = [0u16; 261];
                let ok = GetVolumeInformationW(
                    w.as_ptr(),
                    name.as_mut_ptr(),
                    261,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                );
                let (mut free, mut total, mut all_free) = (0u64, 0u64, 0u64);
                let sized =
                    GetDiskFreeSpaceExW(w.as_ptr(), &mut free, &mut total, &mut all_free) != 0;
                if ok == 0 && !sized && kind != DriveKind::Fixed {
                    // No disk in it: still listed, as Explorer does, but empty.
                }
                drives.push(Drive {
                    root,
                    label: if ok != 0 {
                        from_wide(&name)
                    } else {
                        String::new()
                    },
                    kind,
                    free,
                    total,
                });
            }
            SetErrorMode(old);
            drives
        }
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// Sends files and folders to the Recycle Bin, as Explorer's Delete does.
pub fn recycle(paths: &[String]) -> Result<(), String> {
    #[cfg(windows)]
    {
        use ffi::*;
        if paths.is_empty() {
            return Ok(());
        }
        // A list of paths, each ending in a zero, the whole ending in another.
        let mut from: Vec<u16> = Vec::new();
        for p in paths {
            from.extend(p.encode_utf16());
            from.push(0);
        }
        from.push(0);
        const FO_DELETE: u32 = 3;
        const FOF_SILENT: u16 = 0x4;
        const FOF_NOCONFIRMATION: u16 = 0x10;
        const FOF_ALLOWUNDO: u16 = 0x40;
        const FOF_NOERRORUI: u16 = 0x400;
        let mut op = FileOp {
            window: 0,
            func: FO_DELETE,
            from: from.as_ptr(),
            to: std::ptr::null(),
            flags: FOF_SILENT | FOF_NOCONFIRMATION | FOF_ALLOWUNDO | FOF_NOERRORUI,
            aborted: 0,
            mappings: std::ptr::null_mut(),
            title: std::ptr::null(),
        };
        // SAFETY: the list outlives the call and is double-zero-terminated as required.
        let result = unsafe { SHFileOperationW(&mut op) };
        if result == 0 && op.aborted == 0 {
            Ok(())
        } else {
            Err(format!(
                "Windows couldn't move that to the Recycle Bin (error {result:#x})"
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Err("No Recycle Bin here".into())
    }
}

/// The files on the clipboard (copied in Explorer, or by rdrop), and whether they were cut.
pub fn clipboard_files() -> Option<(Vec<PathBuf>, bool)> {
    #[cfg(windows)]
    {
        use ffi::*;
        // SAFETY: the clipboard is opened and closed around the reads; handles are only read.
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 || OpenClipboard(0) == 0 {
                return None;
            }
            let drop = GetClipboardData(CF_HDROP);
            let mut files = Vec::new();
            if drop != 0 {
                let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
                for i in 0..count {
                    let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0) as usize;
                    let mut buf = vec![0u16; len + 1];
                    DragQueryFileW(drop, i, buf.as_mut_ptr(), (len + 1) as u32);
                    files.push(PathBuf::from(from_wide(&buf)));
                }
            }
            let effect = RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr());
            let mut cut = false;
            let handle = GetClipboardData(effect);
            if handle != 0 {
                let p = GlobalLock(handle);
                if !p.is_null() {
                    cut = *(p as *const u32) & 2 != 0;
                    GlobalUnlock(handle);
                }
            }
            CloseClipboard();
            (!files.is_empty()).then_some((files, cut))
        }
    }
    #[cfg(not(windows))]
    None
}

/// Puts files on the clipboard as Explorer does, so they paste into Explorer too.
pub fn set_clipboard_files(paths: &[PathBuf], cut: bool) -> bool {
    #[cfg(windows)]
    {
        use ffi::*;
        // DROPFILES: where the list starts, a point, whether it's in a client area, and
        // whether the names are wide; then the names, each ending in a zero, and one more.
        let mut names: Vec<u16> = Vec::new();
        for p in paths {
            names.extend(p.display().to_string().encode_utf16());
            names.push(0);
        }
        names.push(0);
        let header = 20usize;
        let bytes = header + names.len() * 2;
        const GMEM_MOVEABLE: u32 = 0x2;
        const GMEM_ZEROINIT: u32 = 0x40;
        // SAFETY: the memory is locked while written, then handed to the clipboard, which
        // owns it from then on.
        unsafe {
            if OpenClipboard(0) == 0 {
                return false;
            }
            EmptyClipboard();
            let mem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes);
            let p = GlobalLock(mem);
            if p.is_null() {
                CloseClipboard();
                return false;
            }
            (p as *mut u32).write_unaligned(header as u32);
            (p.add(16) as *mut u32).write_unaligned(1);
            std::ptr::copy_nonoverlapping(
                names.as_ptr() as *const u8,
                p.add(header),
                names.len() * 2,
            );
            GlobalUnlock(mem);
            let placed = SetClipboardData(CF_HDROP, mem) != 0;
            if !placed {
                GlobalFree(mem);
            }
            let effect = RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr());
            let mem = GlobalAlloc(GMEM_MOVEABLE, 4);
            let p = GlobalLock(mem);
            if !p.is_null() {
                (p as *mut u32).write_unaligned(if cut { 2 } else { 1 | 4 });
                GlobalUnlock(mem);
                if SetClipboardData(effect, mem) == 0 {
                    GlobalFree(mem);
                }
            }
            CloseClipboard();
            placed
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (paths, cut);
        false
    }
}

/// A number that changes whenever anything is put on the clipboard.
pub fn clipboard_stamp() -> u32 {
    #[cfg(windows)]
    // SAFETY: a plain query.
    unsafe {
        ffi::GetClipboardSequenceNumber()
    }
    #[cfg(not(windows))]
    0
}

/// Whether all these keys (virtual-key codes) are held down right now.
pub fn keys_down(keys: &[i32]) -> bool {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetAsyncKeyState(key: i32) -> i16;
        }
        // SAFETY: plain queries with no pointers involved.
        keys.iter().all(|k| unsafe { GetAsyncKeyState(*k) } < 0)
    }
    #[cfg(not(windows))]
    {
        let _ = keys;
        false
    }
}

/// Whether a file or folder here is hidden, as Explorer hides it.
pub fn is_hidden(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x2 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_drive_and_folders_are_found() {
        let drives = drives();
        let c = drives
            .iter()
            .find(|d| d.root == "C:\\")
            .expect("C: is there");
        assert!(c.total > 0 && c.free <= c.total);
        assert!(c.title().ends_with("(C:)"));
        assert!(known_folders().iter().any(|k| k.name == "Desktop"));
    }

    /// By hand only: it leaves a small file in the real Recycle Bin.
    #[test]
    #[ignore]
    fn files_go_to_the_recycle_bin() {
        let dir = std::env::temp_dir().join(format!("rdrop-recycle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bin-me.txt");
        std::fs::write(&file, "x").unwrap();
        recycle(&[file.display().to_string()]).unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
