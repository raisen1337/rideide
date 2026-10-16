//! rterm as Windows' default terminal. When a console program starts on its own (cmd from
//! the Start menu, a script double-clicked in Explorer), Windows runs its console host
//! (Windows Terminal's OpenConsole), which hands the program's pseudo console to the
//! terminal named in `HKCU\Console\%%Startup` through COM's `ITerminalHandoff3`. rterm
//! answers that call and shows the program in a tab, as Windows Terminal does.
//!
//! The COM objects are written out by hand: an `IClassFactory` and the handoff object,
//! both static, in the multithreaded apartment. Calls are marshalled by Windows Terminal's
//! own proxy (OpenConsoleProxy.dll), registered for rterm's side as well when it is made
//! the default.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

type HRESULT = i32;
type HANDLE = isize;

const S_OK: HRESULT = 0;
const E_NOINTERFACE: HRESULT = 0x8000_4002_u32 as i32;
const E_POINTER: HRESULT = 0x8000_4003_u32 as i32;
const E_FAIL: HRESULT = 0x8000_4005_u32 as i32;
const CLASS_E_NOAGGREGATION: HRESULT = 0x8004_0110_u32 as i32;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GUID(u32, u16, u16, [u8; 8]);

/// rterm's handoff class: the id Windows is told to hand consoles to.
pub const CLSID_RTERM: GUID = GUID(
    0x6c8a_2e4b,
    0x7d31,
    0x4f5a,
    [0x9b, 0x0e, 0x52, 0xd4, 0xc1, 0xa7, 0xe3, 0xf9],
);
const CLSID_RTERM_TEXT: &str = "{6C8A2E4B-7D31-4F5A-9B0E-52D4C1A7E3F9}";
const IID_IUNKNOWN: GUID = GUID(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_ICLASSFACTORY: GUID = GUID(1, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_ITERMINALHANDOFF3: GUID = GUID(
    0x6f23_da90,
    0x15c5,
    0x4203,
    [0x9d, 0xb0, 0x64, 0xe7, 0x3f, 0x1b, 0x1b, 0x00],
);
const IID_ITERMINALHANDOFF3_TEXT: &str = "{6F23DA90-15C5-4203-9DB0-64E73F1B1B00}";
/// Windows Terminal's proxy for the handoff interfaces, in OpenConsoleProxy.dll.
const PROXY_CLSID_TEXT: &str = "{3171DE52-6EFA-4AEF-8A9F-D02BD67E7A4F}";
/// Windows Terminal's console host and terminal, to go back to.
const WT_CONSOLE: &str = "{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}";
const WT_TERMINAL: &str = "{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}";

#[repr(C)]
struct StartupInfo {
    title: *const u16,
    icon_path: *const u16,
    icon_index: i32,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_chars: u32,
    y_chars: u32,
    fill: u32,
    flags: u32,
    show: u16,
}

/// A console handed over: the pipes to talk to it, and what keeps it alive.
pub struct Handoff {
    /// What is typed goes in here.
    pub input: File,
    /// What the program shows comes out here.
    pub output: File,
    /// Resizes go here, as Windows' pseudo consoles take them.
    pub signal: File,
    /// Held while the console should live: letting go of them closes it.
    #[allow(dead_code)]
    pub reference: OwnedHandle,
    #[allow(dead_code)]
    pub server: OwnedHandle,
    /// The program itself, to learn when it ends.
    pub client: OwnedHandle,
    pub title: String,
    /// The program's path, to start it again from the tab.
    pub program: String,
}

static HANDOFFS: Mutex<Option<Sender<Handoff>>> = Mutex::new(None);

/// A line in %LOCALAPPDATA%\rterm\handoff.log, to see why a handoff didn't arrive.
pub fn log(line: &str) {
    use std::io::Write;
    let Some(dir) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir).join("rterm");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("handoff.log");
    // Kept short: started again once it grows.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "{secs} [{}] {line}", std::process::id());
    }
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *const c_void, coinit: u32) -> HRESULT;
    fn CoRegisterClassObject(
        clsid: *const GUID,
        unk: *mut c_void,
        context: u32,
        flags: u32,
        register: *mut u32,
    ) -> HRESULT;
}

#[link(name = "oleaut32")]
extern "system" {
    fn SysStringLen(s: *const u16) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> HANDLE;
    fn DuplicateHandle(
        source_process: HANDLE,
        source: HANDLE,
        target_process: HANDLE,
        target: *mut HANDLE,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        security: *const c_void,
    ) -> HANDLE;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: HANDLE,
    ) -> HANDLE;
    fn QueryFullProcessImageNameW(
        process: HANDLE,
        flags: u32,
        name: *mut u16,
        size: *mut u32,
    ) -> i32;
}

// ---- the COM objects ----

#[repr(C)]
struct UnknownVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct FactoryVtbl {
    unknown: UnknownVtbl,
    create_instance: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    lock_server: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
}

#[repr(C)]
struct HandoffVtbl {
    unknown: UnknownVtbl,
    establish: unsafe extern "system" fn(
        *mut c_void,
        *mut HANDLE,
        *mut HANDLE,
        HANDLE,
        HANDLE,
        HANDLE,
        HANDLE,
        *const StartupInfo,
    ) -> HRESULT,
}

#[repr(C)]
struct Object<V: 'static> {
    vtbl: &'static V,
}

// The objects live for the whole run, so counting references has nothing to free.
unsafe extern "system" fn add_ref(_: *mut c_void) -> u32 {
    2
}
unsafe extern "system" fn release(_: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn factory_qi(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    if *iid == IID_IUNKNOWN || *iid == IID_ICLASSFACTORY {
        *out = this;
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn create_instance(
    _: *mut c_void,
    outer: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if !outer.is_null() {
        return CLASS_E_NOAGGREGATION;
    }
    handoff_qi(&HANDOFF_OBJECT as *const _ as *mut c_void, iid, out)
}

unsafe extern "system" fn lock_server(_: *mut c_void, _: i32) -> HRESULT {
    S_OK
}

unsafe extern "system" fn handoff_qi(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    if *iid == IID_IUNKNOWN || *iid == IID_ITERMINALHANDOFF3 {
        *out = this;
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// A handle of ours for one COM gives us only for the length of the call.
unsafe fn own_copy(handle: HANDLE) -> Option<OwnedHandle> {
    const DUPLICATE_SAME_ACCESS: u32 = 2;
    let mut copy: HANDLE = 0;
    let me = GetCurrentProcess();
    (DuplicateHandle(me, handle, me, &mut copy, 0, 0, DUPLICATE_SAME_ACCESS) != 0 && copy != 0)
        .then(|| OwnedHandle::from_raw_handle(copy as RawHandle))
}

/// A one-way pipe: rterm's end, synchronous, and the console's end, overlapped as
/// OpenConsole may want it. `inbound` is true when rterm reads.
unsafe fn pipe(inbound: bool) -> Option<(File, HANDLE)> {
    const PIPE_ACCESS_INBOUND: u32 = 1;
    const PIPE_ACCESS_OUTBOUND: u32 = 2;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    const PIPE_REJECT_REMOTE_CLIENTS: u32 = 8;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    const INVALID: HANDLE = -1;
    let name = wide(&format!(r"\\.\pipe\rterm-{}", crate::pty::session_id()));
    let mode = if inbound {
        PIPE_ACCESS_INBOUND
    } else {
        PIPE_ACCESS_OUTBOUND
    };
    let server = CreateNamedPipeW(
        name.as_ptr(),
        mode | FILE_FLAG_FIRST_PIPE_INSTANCE,
        PIPE_REJECT_REMOTE_CLIENTS,
        1,
        128 * 1024,
        128 * 1024,
        0,
        std::ptr::null(),
    );
    if server == INVALID {
        return None;
    }
    let access = if inbound {
        GENERIC_WRITE | FILE_READ_ATTRIBUTES
    } else {
        GENERIC_READ | FILE_WRITE_ATTRIBUTES
    };
    let client = CreateFileW(
        name.as_ptr(),
        access,
        0,
        std::ptr::null(),
        OPEN_EXISTING,
        FILE_FLAG_OVERLAPPED,
        0,
    );
    let server = File::from_raw_handle(server as RawHandle);
    if client == INVALID {
        return None;
    }
    Some((server, client))
}

unsafe fn image_name(process: HANDLE) -> String {
    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as u32;
    if QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut size) != 0 {
        String::from_utf16_lossy(&buf[..size as usize])
    } else {
        String::new()
    }
}

unsafe extern "system" fn establish(
    _: *mut c_void,
    input: *mut HANDLE,
    output: *mut HANDLE,
    signal: HANDLE,
    reference: HANDLE,
    server: HANDLE,
    client: HANDLE,
    info: *const StartupInfo,
) -> HRESULT {
    log("EstablishPtyHandoff");
    if input.is_null() || output.is_null() || info.is_null() {
        return E_POINTER;
    }
    // The handles COM passed in are closed when the call returns: keep copies.
    let (Some(signal), Some(reference), Some(server), Some(client)) = (
        own_copy(signal),
        own_copy(reference),
        own_copy(server),
        own_copy(client),
    ) else {
        log("couldn't keep the handles");
        return E_FAIL;
    };
    let (Some((to_console, console_in)), Some((from_console, console_out))) =
        (pipe(false), pipe(true))
    else {
        log("couldn't make the pipes");
        return E_FAIL;
    };
    let title = {
        let t = (*info).title;
        if t.is_null() {
            String::new()
        } else {
            String::from_utf16_lossy(std::slice::from_raw_parts(t, SysStringLen(t) as usize))
        }
    };
    let program = image_name(client.as_raw_handle_isize());
    let handoff = Handoff {
        input: to_console,
        output: from_console,
        signal: File::from(signal),
        reference,
        server,
        client,
        title,
        program,
    };
    let sent = HANDOFFS
        .lock()
        .ok()
        .and_then(|tx| tx.as_ref().map(|tx| tx.send(handoff).is_ok()))
        .unwrap_or(false);
    if !sent {
        log("no window to take it");
        return E_FAIL;
    }
    log("handed over");
    // COM gives the console's ends of the pipes to OpenConsole and closes ours.
    *input = console_in;
    *output = console_out;
    S_OK
}

trait RawIsize {
    fn as_raw_handle_isize(&self) -> HANDLE;
}

impl RawIsize for OwnedHandle {
    fn as_raw_handle_isize(&self) -> HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.as_raw_handle() as HANDLE
    }
}

static FACTORY_VTBL: FactoryVtbl = FactoryVtbl {
    unknown: UnknownVtbl {
        query_interface: factory_qi,
        add_ref,
        release,
    },
    create_instance,
    lock_server,
};
static HANDOFF_VTBL: HandoffVtbl = HandoffVtbl {
    unknown: UnknownVtbl {
        query_interface: handoff_qi,
        add_ref,
        release,
    },
    establish,
};
static FACTORY: Object<FactoryVtbl> = Object {
    vtbl: &FACTORY_VTBL,
};
static HANDOFF_OBJECT: Object<HandoffVtbl> = Object {
    vtbl: &HANDOFF_VTBL,
};

// Only ever read, through COM.
unsafe impl<V> Sync for Object<V> {}

/// Starts taking consoles handed over by Windows; they arrive on the receiver. A thread
/// joins the multithreaded apartment and stays in it, so the registration lasts.
pub fn listen(repaint: eframe::egui::Context) -> Receiver<Handoff> {
    let (tx, rx) = channel::<Handoff>();
    let (inner_tx, inner_rx) = channel::<Handoff>();
    *HANDOFFS.lock().unwrap() = Some(inner_tx);
    std::thread::spawn(move || unsafe {
        const COINIT_MULTITHREADED: u32 = 0;
        const CLSCTX_LOCAL_SERVER: u32 = 4;
        const REGCLS_MULTIPLEUSE: u32 = 1;
        let init = CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED);
        let mut cookie = 0u32;
        let registered = CoRegisterClassObject(
            &CLSID_RTERM,
            &FACTORY as *const _ as *mut c_void,
            CLSCTX_LOCAL_SERVER,
            REGCLS_MULTIPLEUSE,
            &mut cookie,
        );
        if started_by_com() || is_default() {
            log(&format!(
                "listening: init {init:#x}, register {registered:#x}, by COM {}",
                started_by_com()
            ));
        }
        // Passes each handoff on and wakes the window for it.
        for handoff in inner_rx {
            if tx.send(handoff).is_err() {
                break;
            }
            repaint.request_repaint();
        }
    });
    rx
}

/// Whether rterm was started by COM to take a console (`-Embedding`).
pub fn started_by_com() -> bool {
    std::env::args()
        .any(|a| a.eq_ignore_ascii_case("-Embedding") || a.eq_ignore_ascii_case("/Embedding"))
}

// ---- being the default ----

#[link(name = "advapi32")]
extern "system" {
    fn RegSetKeyValueW(
        key: isize,
        subkey: *const u16,
        value: *const u16,
        kind: u32,
        data: *const c_void,
        size: u32,
    ) -> i32;
    fn RegGetValueW(
        key: isize,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn RegDeleteTreeW(key: isize, subkey: *const u16) -> i32;
    fn RegOpenKeyExW(
        key: isize,
        subkey: *const u16,
        options: u32,
        access: u32,
        out: *mut isize,
    ) -> i32;
    fn RegEnumKeyExW(
        key: isize,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *const u32,
        class: *mut u16,
        class_len: *mut u32,
        time: *mut u64,
    ) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

const HKCU: isize = 0x8000_0001_u32 as i32 as isize;
const STARTUP: &str = r"Console\%%Startup";

fn set_string(subkey: &str, value: &str, data: &str) -> bool {
    const REG_SZ: u32 = 1;
    let (k, d) = (wide(subkey), wide(data));
    let v = wide(value);
    let value_ptr = if value.is_empty() {
        std::ptr::null()
    } else {
        v.as_ptr()
    };
    unsafe {
        RegSetKeyValueW(
            HKCU,
            k.as_ptr(),
            value_ptr,
            REG_SZ,
            d.as_ptr() as *const c_void,
            (d.len() * 2) as u32,
        ) == 0
    }
}

fn get_string(root: isize, subkey: &str, value: &str) -> Option<String> {
    const RRF_RT_REG_SZ: u32 = 2;
    let (k, v) = (wide(subkey), wide(value));
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            root,
            k.as_ptr(),
            if value.is_empty() {
                std::ptr::null()
            } else {
                v.as_ptr()
            },
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    } == 0;
    ok.then(|| {
        let len = (size as usize / 2).saturating_sub(1);
        String::from_utf16_lossy(&buf[..len])
    })
}

/// Where Windows Terminal is installed, for its proxy: the newest x64 package.
fn terminal_folder() -> Option<String> {
    let base = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";
    let mut key = 0isize;
    let w = wide(base);
    const KEY_READ: u32 = 0x20019;
    if unsafe { RegOpenKeyExW(HKCU, w.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return None;
    }
    let mut found: Vec<String> = Vec::new();
    for index in 0.. {
        let mut name = [0u16; 512];
        let mut len = name.len() as u32;
        let r = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if r != 0 {
            break;
        }
        let name = String::from_utf16_lossy(&name[..len as usize]);
        if name.starts_with("Microsoft.WindowsTerminal_") && name.contains("_x64_") {
            found.push(name);
        }
    }
    unsafe { RegCloseKey(key) };
    // The version is in the name: the highest one is the one installed now.
    found.sort_by_key(|n| {
        n.split('_')
            .nth(1)
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    });
    found.iter().rev().find_map(|name| {
        let folder = get_string(HKCU, &format!(r"{base}\{name}"), "PackageRootFolder")?;
        std::path::Path::new(&folder)
            .join("OpenConsoleProxy.dll")
            .exists()
            .then_some(folder)
    })
}

/// Whether Windows hands consoles to rterm now.
pub fn is_default() -> bool {
    get_string(HKCU, STARTUP, "DelegationTerminal")
        .is_some_and(|t| t.eq_ignore_ascii_case(CLSID_RTERM_TEXT))
}

/// Makes rterm Windows' default terminal, or gives it back to Windows Terminal. Needs
/// Windows Terminal installed: its console host does the handing over.
pub fn set_default(on: bool) -> Result<(), String> {
    let classes = r"Software\Classes";
    if !on {
        for key in [
            format!(r"{classes}\CLSID\{CLSID_RTERM_TEXT}"),
            format!(r"{classes}\CLSID\{PROXY_CLSID_TEXT}"),
            format!(r"{classes}\Interface\{IID_ITERMINALHANDOFF3_TEXT}"),
        ] {
            unsafe {
                RegDeleteTreeW(HKCU, wide(&key).as_ptr());
            }
        }
        // The copies of the proxy go too, those not held by a running rterm.
        if let Some(dir) =
            std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("rterm"))
        {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("OpenConsoleProxy") && name.ends_with(".dll") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        if is_default() {
            set_string(STARTUP, "DelegationConsole", WT_CONSOLE);
            set_string(STARTUP, "DelegationTerminal", WT_TERMINAL);
        }
        return Ok(());
    }
    let folder = terminal_folder().ok_or(
        "Windows Terminal isn't installed: Windows needs its console host to hand consoles to another terminal. Install it from the Microsoft Store, then try again.",
    )?;
    refresh_proxy(&folder);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let server = format!("\"{}\" -Embedding", exe.display());
    let clsid = format!(r"{classes}\CLSID\{CLSID_RTERM_TEXT}");
    let ok = set_string(&clsid, "", "rterm terminal handoff")
        && set_string(&format!(r"{clsid}\LocalServer32"), "", &server)
        && set_string(STARTUP, "DelegationConsole", WT_CONSOLE)
        && set_string(STARTUP, "DelegationTerminal", CLSID_RTERM_TEXT);
    if ok {
        Ok(())
    } else {
        Err("Windows didn't let rterm change the default terminal".into())
    }
}

/// Points the handoff interface's marshalling at Windows Terminal's proxy where it is
/// installed now; an update moves it, so rterm does this each time it starts as default.
fn refresh_proxy(folder: &str) {
    // Programs outside Windows Terminal's package may read its files but not load them,
    // so rterm keeps a copy of the proxy (built from the open-source Windows Terminal) of
    // its own, refreshed when Windows Terminal updates it.
    let source = std::path::Path::new(folder).join("OpenConsoleProxy.dll");
    let Some(dir) =
        std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("rterm"))
    else {
        return;
    };
    let dll = dir.join("OpenConsoleProxy.dll");
    let same = |a: &std::path::Path, b: &std::path::Path| {
        std::fs::read(a)
            .ok()
            .is_some_and(|x| std::fs::read(b).ok().as_ref() == Some(&x))
    };
    if !same(&source, &dll) {
        let _ = std::fs::create_dir_all(&dir);
        // A copy in use by a running rterm can't be replaced: the new one goes beside it.
        if std::fs::copy(&source, &dll).is_err() {
            let fresh = dir.join(format!("OpenConsoleProxy-{}.dll", std::process::id()));
            if std::fs::copy(&source, &fresh).is_err() {
                return;
            }
            return register_proxy(&fresh);
        }
    }
    register_proxy(&dll)
}

fn register_proxy(dll: &std::path::Path) {
    let classes = r"Software\Classes";
    let proxy = format!(r"{classes}\CLSID\{PROXY_CLSID_TEXT}\InprocServer32");
    set_string(&proxy, "", &dll.display().to_string());
    set_string(&proxy, "ThreadingModel", "Both");
    set_string(
        &format!(r"{classes}\Interface\{IID_ITERMINALHANDOFF3_TEXT}"),
        "",
        "ITerminalHandoff3",
    );
    set_string(
        &format!(r"{classes}\Interface\{IID_ITERMINALHANDOFF3_TEXT}\ProxyStubClsid32"),
        "",
        PROXY_CLSID_TEXT,
    );
}

/// When rterm is the default, keeps the registration pointing at this rterm and at
/// Windows Terminal's current proxy.
pub fn keep_registration() {
    if is_default() {
        if let Some(folder) = terminal_folder() {
            refresh_proxy(&folder);
        }
        if let Ok(exe) = std::env::current_exe() {
            let clsid = format!(r"Software\Classes\CLSID\{CLSID_RTERM_TEXT}\LocalServer32");
            set_string(&clsid, "", &format!("\"{}\" -Embedding", exe.display()));
        }
    }
}
