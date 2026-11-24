use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::inference::InferenceError;

#[derive(Clone, Copy)]
pub(super) enum Engine {
    Speech,
    Correction,
}

impl Engine {
    fn name(self) -> &'static str {
        match self {
            Self::Speech => "whisper-server",
            Self::Correction => "llama-server",
        }
    }

    fn variable(self) -> &'static str {
        match self {
            Self::Speech => "V2T_WHISPER_SERVER",
            Self::Correction => "V2T_LLAMA_SERVER",
        }
    }

    pub(super) fn resolve(self) -> Result<PathBuf, InferenceError> {
        let executable = std::env::current_exe().ok();
        let override_path = std::env::var_os(self.variable());
        let search_path = std::env::var_os("PATH");
        #[cfg(target_os = "macos")]
        let search_path = mac_search_path(search_path.as_deref());
        resolve_at(
            self,
            override_path.as_deref(),
            executable.as_deref().and_then(Path::parent),
            search_path.as_deref(),
        )
    }
}

#[cfg(target_os = "macos")]
fn mac_search_path(path: Option<&OsStr>) -> Option<std::ffi::OsString> {
    // Login agents may start with the system PATH before shell setup runs.
    let default = [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    let supplied = path.unwrap_or_default();
    std::env::join_paths(std::env::split_paths(supplied).chain(default)).ok()
}

fn resolve_at(
    engine: Engine,
    override_path: Option<&OsStr>,
    sibling: Option<&Path>,
    search_path: Option<&OsStr>,
) -> Result<PathBuf, InferenceError> {
    if let Some(path) = override_path {
        return executable_at(Path::new(path)).ok_or(match engine {
            Engine::Speech => InferenceError::InvalidInput(
                "V2T_WHISPER_SERVER must point to an executable using an absolute path.",
            ),
            Engine::Correction => InferenceError::InvalidInput(
                "V2T_LLAMA_SERVER must point to an executable using an absolute path.",
            ),
        });
    }
    if let Some(bundled) =
        sibling.and_then(|directory| executable_at(&directory.join(engine.name())))
    {
        return Ok(bundled);
    }
    if let Some(found) = search_path.and_then(|paths| {
        std::env::split_paths(paths)
            .find_map(|directory| executable_at(&directory.join(engine.name())))
    }) {
        return Ok(found);
    }
    Err(match engine {
        Engine::Speech => InferenceError::Unavailable(
            "Could not find whisper-server. Use the bundled worker or set V2T_WHISPER_SERVER.",
        ),
        Engine::Correction => InferenceError::Unavailable(
            "Could not find llama-server. Install it or set V2T_LLAMA_SERVER.",
        ),
    })
}

fn executable_at(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let metadata = path.metadata().ok()?;
    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
        return path.canonicalize().ok();
    }
    None
}

pub(super) fn configure_command(command: &mut Command) {
    // The API keys belong exclusively to the cloud client. The local correction
    // service receives a fresh loopback-only key via its own command arguments.
    for key in [
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "OPENROUTER_KEY_TOKEN",
        "OPENROUTER_KEY",
        "LLAMA_API_KEY",
    ] {
        command.env_remove(key);
    }
    // Keep llama.cpp's environment options from enabling unrelated server
    // endpoints or changing its bind address independently of these arguments.
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(|name| name.starts_with("LLAMA_ARG_"))
        {
            command.env_remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::{Engine, resolve_at};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "v2t-linux-worker-{}",
                super::super::random_hex::<8>().unwrap()
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn executable(&self, place: &str, name: &str, mode: u32) -> PathBuf {
            let path = self.0.join(place).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"test worker").unwrap();
            fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn resolves_override_then_bundled_then_path_executable() {
        let fixture = Fixture::new();
        let explicit = fixture.executable("chosen", "server", 0o700);
        let bundled = fixture.executable("package", "whisper-server", 0o700);
        let fallback = fixture.executable("path", "whisper-server", 0o700);
        let path = std::env::join_paths([fallback.parent().unwrap()]).unwrap();
        let sibling = bundled.parent();
        assert_eq!(
            resolve_at(
                Engine::Speech,
                Some(explicit.as_os_str()),
                sibling,
                Some(&path)
            )
            .unwrap(),
            explicit
        );
        assert_eq!(
            resolve_at(Engine::Speech, None, sibling, Some(&path)).unwrap(),
            bundled
        );
        assert_eq!(
            resolve_at(Engine::Speech, None, None, Some(&path)).unwrap(),
            fallback
        );
    }

    #[test]
    fn rejects_bad_overrides_without_falling_back() {
        let fixture = Fixture::new();
        let bundled = fixture.executable("package", "llama-server", 0o700);
        let non_executable = fixture.executable("unsafe", "server", 0o600);
        let sibling = bundled.parent();
        for bad in [
            OsStr::new("./server"),
            OsStr::new(""),
            non_executable.as_os_str(),
        ] {
            let error = resolve_at(Engine::Correction, Some(bad), sibling, None).unwrap_err();
            assert!(error.to_string().contains("V2T_LLAMA_SERVER"));
        }
    }

    #[test]
    fn excludes_relative_path_entries_and_unexecutable_files() {
        let fixture = Fixture::new();
        let non_executable = fixture.executable("package", "whisper-server", 0o600);
        let paths = [Path::new("."), non_executable.parent().unwrap()];
        let path: OsString = std::env::join_paths(paths).unwrap();
        assert!(resolve_at(Engine::Speech, None, None, Some(&path)).is_err());
    }
}
