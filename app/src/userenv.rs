//! The user's environment as Windows keeps it (system and user variables from the
//! registry, and the profile folders), rather than what the program that started this one
//! passed down. Terminals start their shells with it, as Windows Terminal does, so a ride
//! or rterm started from another tool (a Claude Code session sets NO_COLOR and its own
//! variables, for one) doesn't hand that tool's settings to every shell. Shared by ride's
//! agent, rterm and rkit.

/// The fresh environment, or `None` where it can't be had (and off Windows).
#[cfg(windows)]
pub fn fresh() -> Option<Vec<(String, String)>> {
    use std::ffi::c_void;
    #[link(name = "userenv")]
    extern "system" {
        fn CreateEnvironmentBlock(env: *mut *mut c_void, token: isize, inherit: i32) -> i32;
        fn DestroyEnvironmentBlock(env: *mut c_void) -> i32;
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn CloseHandle(handle: isize) -> i32;
    }
    const TOKEN_QUERY: u32 = 0x8;
    const TOKEN_DUPLICATE: u32 = 0x2;
    const TOKEN_IMPERSONATE: u32 = 0x4;
    unsafe {
        let mut token = 0isize;
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
            &mut token,
        ) == 0
        {
            return None;
        }
        let mut block: *mut c_void = std::ptr::null_mut();
        let ok = CreateEnvironmentBlock(&mut block, token, 0) != 0;
        CloseHandle(token);
        if !ok || block.is_null() {
            return None;
        }
        // NAME=value\0NAME=value\0\0, in UTF-16.
        let mut vars = Vec::new();
        let mut at = block as *const u16;
        loop {
            let mut len = 0;
            while *at.add(len) != 0 {
                len += 1;
            }
            if len == 0 {
                break;
            }
            let entry = String::from_utf16_lossy(std::slice::from_raw_parts(at, len));
            // "=C:=C:\..." entries are the shell's own; a name never starts with "=".
            if let Some((name, value)) = entry.split_once('=').filter(|(n, _)| !n.is_empty()) {
                vars.push((name.to_string(), value.to_string()));
            }
            at = at.add(len + 1);
        }
        DestroyEnvironmentBlock(block);
        (!vars.is_empty()).then_some(vars)
    }
}

#[cfg(not(windows))]
pub fn fresh() -> Option<Vec<(String, String)>> {
    None
}
