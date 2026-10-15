//! Saved connections ("sites"), and the passwords remembered for them, kept encrypted by
//! Windows for this user (DPAPI): another account, or the file copied to another computer,
//! can't read them.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
pub struct Site {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The folder on the server to start in; the user's home when empty.
    pub folder: String,
    /// Its password is kept in the vault, so it isn't asked for.
    pub remember: bool,
}

impl Site {
    /// How the site is known to panes and transfers.
    pub fn key(&self) -> String {
        format!("site:{}", self.name)
    }

    /// What `ssh` gets to reach it.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.port != 0 && self.port != 22 {
            args.extend(["-p".to_string(), self.port.to_string()]);
        }
        if !self.user.trim().is_empty() {
            args.extend(["-l".to_string(), self.user.trim().to_string()]);
        }
        args.push(self.host.trim().to_string());
        args
    }

    /// user@host, as people write it.
    pub fn address(&self) -> String {
        let port = if self.port != 0 && self.port != 22 {
            format!(":{}", self.port)
        } else {
            String::new()
        };
        if self.user.trim().is_empty() {
            format!("{}{port}", self.host.trim())
        } else {
            format!("{}@{}{port}", self.user.trim(), self.host.trim())
        }
    }
}

fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("rdrop"))
}

pub fn load() -> Vec<Site> {
    let mut sites: Vec<Site> = dir()
        .and_then(|d| std::fs::read(d.join("sites.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    sites.sort_by_key(|s| s.name.to_lowercase());
    sites
}

pub fn save(sites: &[Site]) {
    let Some(dir) = dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_vec_pretty(sites) {
        let _ = std::fs::write(dir.join("sites.json"), json);
    }
}

// ---- the vault ----

fn vault_path() -> Option<PathBuf> {
    Some(dir()?.join("passwords.json"))
}

fn vault() -> HashMap<String, String> {
    vault_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_vault(v: &HashMap<String, String>) {
    let Some(path) = vault_path() else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    if let Ok(json) = serde_json::to_vec_pretty(v) {
        let _ = std::fs::write(path, json);
    }
}

/// The password remembered for a connection, if any.
pub fn password(key: &str) -> Option<String> {
    let sealed = vault().get(key)?.clone();
    let bytes = unseal(&from_hex(&sealed)?)?;
    String::from_utf8(bytes).ok()
}

pub fn remember(key: &str, password: &str) {
    let Some(sealed) = seal(password.as_bytes()) else {
        return;
    };
    let mut v = vault();
    v.insert(key.to_string(), hex(&sealed));
    write_vault(&v);
}

pub fn forget(key: &str) {
    let mut v = vault();
    if v.remove(key).is_some() {
        write_vault(&v);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(windows)]
mod dpapi {
    #[repr(C)]
    pub struct Blob {
        pub len: u32,
        pub data: *mut u8,
    }

    #[link(name = "crypt32")]
    extern "system" {
        pub fn CryptProtectData(
            input: *const Blob,
            description: *const u16,
            entropy: *const Blob,
            reserved: *const std::ffi::c_void,
            prompt: *const std::ffi::c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
        pub fn CryptUnprotectData(
            input: *const Blob,
            description: *mut *mut u16,
            entropy: *const Blob,
            reserved: *const std::ffi::c_void,
            prompt: *const std::ffi::c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }

    /// Never shows a window of its own.
    pub const UI_FORBIDDEN: u32 = 0x1;
}

#[cfg(windows)]
fn crypt(data: &[u8], seal: bool) -> Option<Vec<u8>> {
    use dpapi::*;
    let mut input = data.to_vec();
    let blob = Blob {
        len: input.len() as u32,
        data: input.as_mut_ptr(),
    };
    let mut out = Blob {
        len: 0,
        data: std::ptr::null_mut(),
    };
    let (no_text, no_blob, nothing) = (
        std::ptr::null::<u16>(),
        std::ptr::null::<Blob>(),
        std::ptr::null::<std::ffi::c_void>(),
    );
    // SAFETY: the input outlives the call; the output is copied out and then freed with
    // LocalFree, as the documentation says.
    unsafe {
        let ok = if seal {
            CryptProtectData(
                &blob,
                no_text,
                no_blob,
                nothing,
                nothing,
                UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &blob,
                std::ptr::null_mut(),
                no_blob,
                nothing,
                nothing,
                UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 || out.data.is_null() {
            return None;
        }
        let result = std::slice::from_raw_parts(out.data, out.len as usize).to_vec();
        LocalFree(out.data.cast());
        Some(result)
    }
}

#[cfg(windows)]
fn seal(data: &[u8]) -> Option<Vec<u8>> {
    crypt(data, true)
}

#[cfg(windows)]
fn unseal(data: &[u8]) -> Option<Vec<u8>> {
    crypt(data, false)
}

#[cfg(not(windows))]
fn seal(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(windows))]
fn unseal(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_seal_and_unseal() {
        let sealed = seal("hunter2 ünïcode".as_bytes()).unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(unseal(&sealed).unwrap(), "hunter2 ünïcode".as_bytes());
        assert_eq!(from_hex(&hex(&sealed)).unwrap(), sealed);
    }

    #[test]
    fn sites_reach_ssh_as_written() {
        let site = Site {
            name: "shop".into(),
            host: "example.com".into(),
            port: 2222,
            user: "deploy".into(),
            ..Site::default()
        };
        assert_eq!(
            site.ssh_args(),
            ["-p", "2222", "-l", "deploy", "example.com"]
        );
        assert_eq!(site.address(), "deploy@example.com:2222");
        assert_eq!(site.key(), "site:shop");
    }
}
