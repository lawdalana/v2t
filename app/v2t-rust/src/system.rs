use crate::resource::Snapshot;
use std::{
    ffi::c_void,
    io, mem,
    ptr::{null, null_mut},
    slice,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, FreeLibrary, GetLastError, HANDLE, HMODULE, HWND,
            LocalFree,
        },
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
        System::{
            LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::{
                CreateMutexW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                QueryFullProcessImageNameW,
            },
        },
        UI::{
            Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState},
            WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
        },
    },
    w,
};
use zeroize::{Zeroize, Zeroizing};

pub struct Instance(HANDLE);
impl Instance {
    pub fn acquire() -> io::Result<Option<Self>> {
        // SAFETY: a static NUL-terminated name and default security attributes are valid.
        let (handle, already_exists) = unsafe {
            let handle = CreateMutexW(null(), 0, w!("Local\\v2t-desktop"));
            (handle, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let owner = Self(handle);
        Ok((!already_exists).then_some(owner))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: owned kernel handle returned by CreateMutexW.
        unsafe { CloseHandle(self.0) };
    }
}

struct Blob(CRYPT_INTEGER_BLOB);
impl Blob {
    fn bytes(&self) -> &[u8] {
        if self.0.cbData == 0 {
            return &[];
        }
        // SAFETY: DPAPI returns a readable buffer of the stated length on success.
        unsafe { slice::from_raw_parts(self.0.pbData, self.0.cbData as usize) }
    }
}

impl Drop for Blob {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            // SAFETY: the DPAPI allocation is exclusively owned by this blob.
            unsafe {
                slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize).zeroize();
                LocalFree(self.0.pbData.cast());
            }
        }
    }
}

fn dpapi(bytes: &[u8], encrypt: bool) -> io::Result<Blob> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| io::Error::other("API key exceeds storage limit."))?,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = Blob(CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    });
    // SAFETY: input points to live read-only data, output is initialized writable storage.
    // DPAPI uses the current Windows user when no machine scope flag or entropy is supplied.
    let result = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                null(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        } else {
            CryptUnprotectData(
                &input,
                null_mut(),
                null(),
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        }
    };
    if result == 0 {
        return Err(io::Error::other(
            "Windows could not access the encrypted API key.",
        ));
    }
    Ok(output)
}

pub fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    Ok(dpapi(bytes, true)?.bytes().to_vec())
}

pub fn unprotect(bytes: &[u8]) -> io::Result<Zeroizing<String>> {
    let plaintext = dpapi(bytes, false)?;
    let text = str::from_utf8(plaintext.bytes())
        .map_err(|_| io::Error::other("The saved API key is invalid."))?;
    Ok(Zeroizing::new(text.into()))
}

#[derive(Default)]
pub struct Probe {
    nvml: Option<Nvml>,
    next_gpu_probe: Option<Instant>,
}

impl Probe {
    pub fn capture(&mut self, hwnd: isize, measure_gpu: bool) -> Snapshot {
        if self.nvml.is_none()
            && self
                .next_gpu_probe
                .is_none_or(|until| Instant::now() >= until)
        {
            self.nvml = Nvml::load();
            self.next_gpu_probe = Some(Instant::now() + Duration::from_secs(30));
        }
        let mut memory = MEMORYSTATUSEX {
            dwLength: mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: GlobalMemoryStatusEx is provided with a writable struct of the correct size.
        let ram =
            (unsafe { GlobalMemoryStatusEx(&mut memory) } != 0).then_some(memory.ullAvailPhys);
        Snapshot {
            ram,
            vram: self.nvml.as_ref().and_then(Nvml::free),
            gpu: if measure_gpu {
                self.nvml.as_ref().and_then(Nvml::usage)
            } else {
                None
            },
            game: foreground_game(hwnd as HWND),
        }
    }
}

fn foreground_game(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut state = 0;
    let mut id = 0;
    // SAFETY: HWND is a Windows token, all output pointers refer to writable stack storage.
    unsafe {
        if hwnd == GetForegroundWindow()
            && SHQueryUserNotificationState(&mut state) >= 0
            && state == QUNS_RUNNING_D3D_FULL_SCREEN
        {
            return true;
        }
        if GetWindowThreadProcessId(hwnd, &mut id) == 0 {
            return false;
        }
    }
    // SAFETY: request only the process query right. The returned handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if process.is_null() {
        return false;
    }
    let mut length = 32768u32;
    let mut path = vec![0u16; length as usize];
    // SAFETY: path contains length writable UTF-16 units, and the handle is open.
    let query = unsafe {
        let success = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length);
        CloseHandle(process);
        success
    };
    query != 0 && game_path(&String::from_utf16_lossy(&path[..length as usize]))
}

fn game_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("\\steamapps\\common\\")
        || (path.contains("\\epic games\\")
            && !path.contains("\\epic games\\launcher\\")
            && !path.contains("\\epic games\\ue_"))
        || (path.contains("\\riot games\\") && !path.contains("\\riot games\\riot client\\"))
        || path.contains("\\gog galaxy\\games\\")
        || path.contains("\\gog games\\")
}

#[repr(C)]
#[derive(Default)]
struct GpuMemory {
    total: u64,
    free: u64,
    used: u64,
}
#[repr(C)]
#[derive(Default)]
struct GpuUtilization {
    gpu: u32,
    memory: u32,
}

type GpuDevice = *mut c_void;
type Shutdown = unsafe extern "C" fn() -> i32;
type Memory = unsafe extern "C" fn(GpuDevice, *mut GpuMemory) -> i32;
type Utilization = unsafe extern "C" fn(GpuDevice, *mut GpuUtilization) -> i32;

struct Driver(HMODULE);
impl Drop for Driver {
    fn drop(&mut self) {
        // SAFETY: one owned reference from LoadLibraryExW.
        unsafe { FreeLibrary(self.0) };
    }
}

struct Nvml {
    _driver: Driver,
    device: GpuDevice,
    shutdown: Shutdown,
    memory: Memory,
    utilization: Utilization,
}

impl Nvml {
    fn load() -> Option<Self> {
        // SAFETY: load only the installed driver from System32, never a working-directory DLL.
        let module =
            unsafe { LoadLibraryExW(w!("nvml.dll"), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module.is_null() {
            return None;
        }
        let driver = Driver(module);
        // SAFETY: these names are documented NVML exports with the following C ABI signatures.
        unsafe {
            let init: unsafe extern "C" fn() -> i32 =
                mem::transmute(GetProcAddress(module, c"nvmlInit_v2".as_ptr().cast())?);
            let shutdown: Shutdown =
                mem::transmute(GetProcAddress(module, c"nvmlShutdown".as_ptr().cast())?);
            let get_device: unsafe extern "C" fn(u32, *mut GpuDevice) -> i32 = mem::transmute(
                GetProcAddress(module, c"nvmlDeviceGetHandleByIndex_v2".as_ptr().cast())?,
            );
            let memory: Memory = mem::transmute(GetProcAddress(
                module,
                c"nvmlDeviceGetMemoryInfo".as_ptr().cast(),
            )?);
            let utilization: Utilization = mem::transmute(GetProcAddress(
                module,
                c"nvmlDeviceGetUtilizationRates".as_ptr().cast(),
            )?);
            if init() != 0 {
                return None;
            }
            let mut device = null_mut();
            if get_device(0, &mut device) != 0 || device.is_null() {
                shutdown();
                return None;
            }
            Some(Self {
                _driver: driver,
                device,
                shutdown,
                memory,
                utilization,
            })
        }
    }

    fn free(&self) -> Option<u64> {
        let mut memory = GpuMemory::default();
        // SAFETY: device is valid as long as this instance holds the initialized driver.
        ((unsafe { (self.memory)(self.device, &mut memory) } == 0) && memory.free <= memory.total)
            .then_some(memory.free)
    }

    fn usage(&self) -> Option<u32> {
        let mut utilization = GpuUtilization::default();
        // SAFETY: device is valid and utilization is writable for both C fields.
        ((unsafe { (self.utilization)(self.device, &mut utilization) } == 0)
            && utilization.gpu <= 100)
            .then_some(utilization.gpu)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: shuts down one initialization before Driver releases the module.
        unsafe { (self.shutdown)() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dpapi_round_trips_current_windows_user() {
        let ciphertext = protect(b"synthetic-test-value").unwrap();
        assert_ne!(ciphertext, b"synthetic-test-value");
        assert_eq!(
            unprotect(&ciphertext).unwrap().as_str(),
            "synthetic-test-value"
        );
        assert!(unprotect(b"not a dpapi blob").is_err());
    }
    #[test]
    fn detects_game_folders_without_matching_launchers() {
        assert!(game_path(
            "D:\\SteamLibrary\\SteamApps\\common\\Title\\game.exe"
        ));
        assert!(!game_path("D:\\Epic Games\\Launcher\\game.exe"));
    }
}
