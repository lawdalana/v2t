use std::{
    ffi::{OsStr, OsString},
    fs, io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::null_mut,
};
use windows::{
    Win32::{
        Storage::FileSystem::MoveFileW,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
        },
        UI::Shell::{
            FOLDERID_Startup, IShellLinkW, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath, SLGP_RAWPATH,
            ShellLink,
        },
    },
    core::{Interface, PCWSTR},
};

const LINK_NAME: &str = "v2t.lnk";
const BACKGROUND_ARG: &str = "--background";
const DESCRIPTION: &str = "v2t login launcher";

pub struct Startup {
    executable: PathBuf,
}

impl Startup {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    pub fn enabled(&self) -> io::Result<bool> {
        self.enabled_at(&startup_folder()?)
    }

    pub fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        self.set_enabled_at(&startup_folder()?, enabled)
    }

    fn enabled_at(&self, folder: &Path) -> io::Result<bool> {
        Ok(read_link(&folder.join(LINK_NAME))?.is_some_and(|link| {
            link.starts(&self.executable) || (link.is_managed() && link.executable.is_file())
        }))
    }

    fn owns(&self, link: &Registration) -> bool {
        link.starts(&self.executable) || link.is_managed()
    }

    fn set_enabled_at(&self, folder: &Path, enabled: bool) -> io::Result<()> {
        let path = folder.join(LINK_NAME);
        let previous = read_link(&path)?;

        if !enabled {
            if previous.is_some_and(|link| self.owns(&link)) {
                match fs::remove_file(&path) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    result => result?,
                }
            }
            return Ok(());
        }

        if let Some(link) = &previous {
            if link.starts(&self.executable) {
                return Ok(());
            }
            if !self.owns(link) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "An existing v2t.lnk in the Windows Startup folder points to a different app.",
                ));
            }
        }

        fs::create_dir_all(folder)?;
        let temporary = folder.join(format!(".v2t-{}.tmp", nonce()?));
        let result = write_link(&temporary, &self.executable, BACKGROUND_ARG, DESCRIPTION)
            .and_then(|()| {
                if previous.is_some() {
                    fs::rename(&temporary, &path)
                } else {
                    // SAFETY: both buffers are terminated UTF-16 strings. MoveFileW
                    // leaves a destination that appeared after inspection untouched.
                    let source = wide(temporary.as_os_str());
                    let destination = wide(path.as_os_str());
                    unsafe { MoveFileW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr())) }?;
                    Ok(())
                }
            });
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

struct Registration {
    executable: PathBuf,
    arguments: String,
    description: String,
}

impl Registration {
    fn starts(&self, executable: &Path) -> bool {
        self.arguments.trim() == BACKGROUND_ARG
            && self
                .executable
                .as_os_str()
                .eq_ignore_ascii_case(executable.as_os_str())
    }

    fn is_managed(&self) -> bool {
        self.arguments.trim() == BACKGROUND_ARG && self.description == DESCRIPTION
    }
}

fn startup_folder() -> io::Result<PathBuf> {
    // SAFETY: the constant identifies the per-user Startup folder. The returned
    // UTF-16 path belongs to this call and must be freed with CoTaskMemFree.
    unsafe {
        let directory = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DONT_VERIFY, None)
            .map_err(io::Error::other)?;
        let path = PathBuf::from(OsString::from_wide(directory.as_wide()));
        CoTaskMemFree(Some(directory.0.cast()));
        Ok(path)
    }
}

fn read_link(path: &Path) -> io::Result<Option<Registration>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(io::Error::other(
                "The existing Windows Startup shortcut is not a regular file.",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }

    let _apartment = Apartment::new()?;
    let link = shell_link()?;
    let file: IPersistFile = link.cast().map_err(io::Error::other)?;
    let path = wide(path.as_os_str());
    let mut executable = vec![0; 32_768];
    let mut arguments = [0; 1024];
    let mut description = [0; 1024];
    // SAFETY: the COM interfaces are live in this apartment, the path is a
    // terminated wide string, and the output buffers are writable. RAWPATH
    // reads the supplied link without resolving the target.
    unsafe {
        file.Load(PCWSTR(path.as_ptr()), STGM_READ)?;
        link.GetPath(&mut executable, null_mut(), SLGP_RAWPATH.0 as u32)?;
        link.GetArguments(&mut arguments)?;
        link.GetDescription(&mut description)?;
    }
    Ok(Some(Registration {
        executable: PathBuf::from(OsString::from_wide(until_nul(&executable))),
        arguments: String::from_utf16_lossy(until_nul(&arguments)),
        description: String::from_utf16_lossy(until_nul(&description)),
    }))
}

fn write_link(
    path: &Path,
    executable: &Path,
    arguments: &str,
    description: &str,
) -> io::Result<()> {
    let _apartment = Apartment::new()?;
    let link = shell_link()?;
    let file: IPersistFile = link.cast().map_err(io::Error::other)?;
    let target = wide(executable.as_os_str());
    let directory = wide(executable.parent().unwrap_or(Path::new("")).as_os_str());
    let arguments = wide(arguments.as_ref());
    let description = wide(description.as_ref());
    let path = wide(path.as_os_str());
    // SAFETY: the COM interfaces belong to this thread, and each argument is a
    // terminated UTF-16 string that remains allocated during the call.
    unsafe {
        link.SetPath(PCWSTR(target.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(directory.as_ptr()))?;
        link.SetArguments(PCWSTR(arguments.as_ptr()))?;
        link.SetDescription(PCWSTR(description.as_ptr()))?;
        file.Save(PCWSTR(path.as_ptr()), false)?;
    }
    Ok(())
}

pub(crate) struct Apartment;

impl Apartment {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: this operates on the current thread. Drop balances success.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.map_err(io::Error::other)?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: an instance is created after successful initialization, and
        // the callers drop it in the same thread after releasing their interfaces.
        unsafe { CoUninitialize() };
    }
}

fn shell_link() -> io::Result<IShellLinkW> {
    // SAFETY: the calling thread initialized COM and the CLSID identifies the
    // system Shell Link object. An in-process, unaggregated instance is requested.
    unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.map_err(io::Error::other)
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

fn until_nul(chars: &[u16]) -> &[u16] {
    &chars[..chars
        .iter()
        .position(|&character| character == 0)
        .unwrap_or(chars.len())]
}

fn nonce() -> io::Result<String> {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes)
        .map_err(|_| io::Error::other("Could not prepare a temporary Windows Startup shortcut."))?;
    Ok(format!("{:016x}", u64::from_ne_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn startup_checkbox_recognizes_legacy_link_and_can_disable_it() {
        let folder = TempFolder::create();
        let exe = env::current_exe().unwrap();
        let startup = Startup::new(exe.clone());
        assert!(!startup.enabled_at(&folder.0).unwrap());

        write_link(&folder.0.join(LINK_NAME), &exe, "--background", "").unwrap();
        assert!(startup.enabled_at(&folder.0).unwrap());
        startup.set_enabled_at(&folder.0, false).unwrap();
        assert!(!folder.0.join(LINK_NAME).exists());
    }

    #[test]
    fn new_registration_targets_this_executable_in_background() {
        let folder = TempFolder::create();
        let exe = env::current_exe().unwrap();
        let startup = Startup::new(exe.clone());
        startup.set_enabled_at(&folder.0, true).unwrap();
        let link = read_link(&folder.0.join(LINK_NAME)).unwrap().unwrap();
        assert!(link.starts(&exe));
        assert!(link.is_managed());
        startup.set_enabled_at(&folder.0, true).unwrap();
        startup.set_enabled_at(&folder.0, false).unwrap();
        startup.set_enabled_at(&folder.0, false).unwrap();
        assert!(!folder.0.join(LINK_NAME).exists());
    }

    #[test]
    fn unrelated_shortcuts_are_never_replaced_or_removed() {
        let folder = TempFolder::create();
        let exe = env::current_exe().unwrap();
        let startup = Startup::new(exe.clone());
        let path = folder.0.join(LINK_NAME);
        write_link(&path, &exe, "--unrelated", "another startup task").unwrap();
        let original = fs::read(&path).unwrap();
        assert!(!startup.enabled_at(&folder.0).unwrap());
        assert_eq!(
            startup.set_enabled_at(&folder.0, true).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        startup.set_enabled_at(&folder.0, false).unwrap();
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn moving_the_app_leaves_a_managed_shortcut_that_can_be_repaired() {
        let folder = TempFolder::create();
        let old_executable = folder.0.join("old v2t.exe");
        fs::write(&old_executable, []).unwrap();
        let original = Startup::new(old_executable.clone());
        original.set_enabled_at(&folder.0, true).unwrap();
        fs::remove_file(old_executable).unwrap();

        let exe = env::current_exe().unwrap();
        let startup = Startup::new(exe.clone());
        assert!(!startup.enabled_at(&folder.0).unwrap());
        startup.set_enabled_at(&folder.0, true).unwrap();
        let link = read_link(&folder.0.join(LINK_NAME)).unwrap().unwrap();
        assert!(link.starts(&exe));
    }

    struct TempFolder(PathBuf);

    impl TempFolder {
        fn create() -> Self {
            let folder = env::temp_dir().join(format!("v2t-startup ทดสอบ-{}", nonce().unwrap()));
            fs::create_dir(&folder).unwrap();
            Self(folder)
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
