use crate::config::{atomic_write, read_bounded};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ENTRY_LIMIT: u64 = 32 * 1024;
const CHECKBOX: &str = "Start V2T At Startup";
const DESKTOP_MARKER: &str = "X-v2t-Managed";
const PLIST_PREFIX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Managed by v2t -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>io.github.lawdalana.v2t</string>
  <key>ProgramArguments</key>
  <array>
    <string>"#;
const PLIST_SUFFIX: &str = r#"</string>
    <string>--background</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
</dict>
</plist>
"#;

#[derive(Clone, Copy)]
enum Format {
    DesktopEntry,
    LaunchAgent,
}

struct Registration {
    format: Format,
    file: PathBuf,
    executable: PathBuf,
}

impl Registration {
    fn current() -> io::Result<Self> {
        let home = env::var_os("HOME").map(PathBuf::from);
        let xdg_config = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let format = if cfg!(target_os = "macos") {
            Format::LaunchAgent
        } else {
            Format::DesktopEntry
        };
        Ok(Self {
            format,
            file: format.path(home.as_deref(), xdg_config.as_deref())?,
            executable: env::current_exe()?,
        })
    }

    fn enabled(&self) -> io::Result<bool> {
        let Some(contents) = self.read()? else {
            return Ok(false);
        };
        match self.format {
            Format::DesktopEntry => Ok(self.format.manages(&contents)
                && desktop_value(&contents, "Exec") == Some(&desktop_exec(&self.executable)?)
                && desktop_value(&contents, "Hidden") != Some("true")
                && desktop_value(&contents, "X-GNOME-Autostart-enabled") != Some("false")
                && desktop_value(&contents, "X-MATE-Autostart-enabled") != Some("false")),
            Format::LaunchAgent => Ok(contents == launch_agent(&self.executable)?),
        }
    }

    fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        let contents = self.read()?;
        if contents
            .as_deref()
            .is_some_and(|entry| !self.format.manages(entry))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "A startup entry at the v2t path belongs to another application or was edited.",
            ));
        }
        if enabled {
            let entry = match self.format {
                Format::DesktopEntry => format!(
                    "[Desktop Entry]\nType=Application\nName=v2t\n{DESKTOP_MARKER}=true\nExec={}\nTerminal=false\n",
                    desktop_exec(&self.executable)?
                ),
                Format::LaunchAgent => launch_agent(&self.executable)?,
            };
            atomic_write(&self.file, entry.as_bytes())
        } else if contents.is_some() {
            match fs::remove_file(&self.file) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        } else {
            Ok(())
        }
    }

    fn read(&self) -> io::Result<Option<String>> {
        match fs::symlink_metadata(&self.file) {
            Ok(meta) if meta.is_file() => (),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The v2t startup entry must be a regular file.",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        read_bounded(&self.file, ENTRY_LIMIT)?
            .map(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.utf8_error()))
            })
            .transpose()
    }
}

impl Format {
    fn path(self, home: Option<&Path>, xdg_config: Option<&Path>) -> io::Result<PathBuf> {
        let home = home.filter(|path| path.is_absolute());
        let base = match self {
            Self::DesktopEntry => xdg_config
                .filter(|path| path.is_absolute())
                .map(Path::to_path_buf)
                .or_else(|| home.map(|path| path.join(".config"))),
            Self::LaunchAgent => home.map(|path| path.join("Library")),
        }
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "The login configuration directory is unavailable.",
            )
        })?;
        let path = match self {
            Self::DesktopEntry => "autostart/io.github.lawdalana.v2t.desktop",
            Self::LaunchAgent => "LaunchAgents/io.github.lawdalana.v2t.plist",
        };
        Ok(base.join(path))
    }

    fn manages(self, entry: &str) -> bool {
        match self {
            Self::DesktopEntry => {
                desktop_value(entry, "Name") == Some("v2t")
                    && desktop_value(entry, DESKTOP_MARKER) == Some("true")
                    && desktop_value(entry, "Type") == Some("Application")
            }
            Self::LaunchAgent => entry.starts_with(PLIST_PREFIX) && entry.ends_with(PLIST_SUFFIX),
        }
    }
}

fn desktop_value<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    let mut values = entry
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "[Desktop Entry]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| *name == key)
        .map(|(_, value)| value);
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn launch_path(executable: &Path) -> io::Result<&str> {
    executable
        .to_str()
        .filter(|value| executable.is_absolute() && !value.chars().any(char::is_control))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "The executable path cannot be represented in a login entry.",
            )
        })
}

fn desktop_exec(executable: &Path) -> io::Result<String> {
    let executable = launch_path(executable)?;
    if executable.contains('=') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The executable path cannot contain '=' in a desktop entry.",
        ));
    }
    let mut value = if executable.contains('%') {
        // GIO checks the first token before expanding percent codes.
        String::from("/usr/bin/env -- \"")
    } else {
        String::from("\"")
    };
    for ch in executable.chars() {
        match ch {
            '%' => value.push_str("%%"),
            '\\' => value.push_str(r"\\\\"),
            '"' | '$' | '`' => {
                value.push_str(r"\\");
                value.push(ch);
            }
            _ => value.push(ch),
        }
    }
    value.push_str("\" --background");
    Ok(value)
}

fn launch_agent(executable: &Path) -> io::Result<String> {
    let executable = launch_path(executable)?;
    let mut xml = String::from(PLIST_PREFIX);
    for ch in executable.chars() {
        match ch {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '"' => xml.push_str("&quot;"),
            '\'' => xml.push_str("&apos;"),
            // XML 1.0 reserves these code points.
            '\u{fffe}' | '\u{ffff}' => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "The executable path cannot be represented in a LaunchAgent.",
                ));
            }
            _ => xml.push(ch),
        }
    }
    xml.push_str(PLIST_SUFFIX);
    Ok(xml)
}

pub(super) fn enabled() -> io::Result<bool> {
    Registration::current()?.enabled()
}

pub(super) fn set_enabled(enabled: bool) -> io::Result<()> {
    Registration::current()?.set_enabled(enabled)
}

pub(super) fn edit_settings() -> io::Result<()> {
    let registration = Registration::current()?;
    let previous = registration.enabled()?;
    if let Some(updated) = dialog(previous)?
        && updated != previous
    {
        registration.set_enabled(updated)?;
    }
    Ok(())
}

fn run_dialog(command: &mut Command) -> io::Result<Output> {
    for key in [
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "OPENROUTER_KEY_TOKEN",
        "OPENROUTER_KEY",
    ] {
        command.env_remove(key);
    }
    command.output()
}

#[cfg(target_os = "linux")]
fn dialog(enabled: bool) -> io::Result<Option<bool>> {
    let result = run_dialog(Command::new("zenity").args([
        "--list",
        "--checklist",
        "--title=v2t Settings",
        "--text=Start v2t in the background when you log in.",
        "--hide-header",
        "--column=Enabled",
        "--column=Setting",
        "--print-column=2",
        "--ok-label=Save",
        "--cancel-label=Cancel",
        if enabled { "TRUE" } else { "FALSE" },
        CHECKBOX,
    ]))
    .map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            io::Error::new(
                error.kind(),
                "Install Zenity to open v2t Settings, or use the v2t startup command.",
            )
        } else {
            error
        }
    })?;
    if result.status.success() {
        match str::from_utf8(&result.stdout).ok().map(str::trim) {
            Some(CHECKBOX) => Ok(Some(true)),
            Some("") => Ok(Some(false)),
            _ => Err(io::Error::other(
                "The v2t Settings dialog returned an invalid choice.",
            )),
        }
    } else if result.status.code() == Some(1) && result.stderr.is_empty() {
        Ok(None)
    } else {
        Err(io::Error::other(
            "Could not open v2t Settings. Run it in a desktop session or use the v2t startup command.",
        ))
    }
}

#[cfg(target_os = "macos")]
fn dialog(enabled: bool) -> io::Result<Option<bool>> {
    // Pass the current selection as an argument, keeping the script independent of user input.
    const SCRIPT: &str = r#"
ObjC.import('AppKit');
function run(args) {
    var app = $.NSApplication.sharedApplication;
    app.setActivationPolicy($.NSApplicationActivationPolicyAccessory);
    app.activateIgnoringOtherApps(true);
    var alert = $.NSAlert.alloc.init;
    alert.messageText = 'v2t Settings';
    alert.informativeText = 'Start v2t in the background when you log in.';
    alert.addButtonWithTitle('Save');
    alert.addButtonWithTitle('Cancel');
    alert.showsSuppressionButton = true;
    var checkbox = alert.suppressionButton;
    checkbox.title = args[1];
    checkbox.state = args[0] === 'true' ? 1 : 0;
    if (alert.runModal !== $.NSAlertFirstButtonReturn) return 'cancel';
    return checkbox.state === 1 ? 'enabled' : 'disabled';
}
"#;
    let result = run_dialog(Command::new("/usr/bin/osascript").args([
        "-l",
        "JavaScript",
        "-e",
        SCRIPT,
        if enabled { "true" } else { "false" },
        CHECKBOX,
    ]))?;
    if !result.status.success() {
        return Err(io::Error::other(
            "Could not open v2t Settings. Run it in a desktop session or use the v2t startup command.",
        ));
    }
    match str::from_utf8(&result.stdout).ok().map(str::trim) {
        Some("enabled") => Ok(Some(true)),
        Some("disabled") => Ok(Some(false)),
        Some("cancel") => Ok(None),
        _ => Err(io::Error::other(
            "The v2t Settings dialog returned an invalid choice.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let directory = env::temp_dir().join(format!(
                "v2t-startup-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&directory).expect("temporary test directory");
            Self(directory)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn desktop_entry_quotes_the_path_and_preserves_literal_percent() {
        let executable = Path::new(r#"/opt/my apps/$x " ` \ 100%/v2t"#);
        assert_eq!(
            desktop_exec(executable).unwrap(),
            r#"/usr/bin/env -- "/opt/my apps/\\$x \\" \\` \\\\ 100%%/v2t" --background"#,
        );
        assert!(desktop_exec(Path::new("/tmp/a=v2t")).is_err());
        assert!(desktop_exec(Path::new("/tmp/a\n[v2t]")).is_err());
        assert!(desktop_exec(Path::new("relative/v2t")).is_err());
    }

    #[test]
    fn launch_agent_preserves_path_and_argument_boundaries() {
        let plist = launch_agent(Path::new("/Applications/a & b <'quoted'>/v2t")).unwrap();
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(
            plist.contains(
                "<string>/Applications/a &amp; b &lt;&apos;quoted&apos;&gt;/v2t</string>"
            )
        );
        assert!(plist.contains("<string>--background</string>"));
    }

    #[test]
    fn toggles_both_formats_idempotently_and_updates_moved_binary() {
        for format in [Format::DesktopEntry, Format::LaunchAgent] {
            let directory = Scratch::new();
            let mut entry = Registration {
                format,
                file: directory.0.join("autostart").join("v2t-test"),
                executable: PathBuf::from("/opt/v2t"),
            };
            assert!(!entry.enabled().unwrap());
            entry.set_enabled(true).unwrap();
            entry.set_enabled(true).unwrap();
            assert!(entry.enabled().unwrap());
            entry.executable = PathBuf::from("/usr/local/bin/v2t");
            assert!(!entry.enabled().unwrap());
            entry.set_enabled(true).unwrap();
            assert!(entry.enabled().unwrap());
            entry.set_enabled(false).unwrap();
            entry.set_enabled(false).unwrap();
            assert!(!entry.file.exists());
        }
    }

    #[test]
    fn leaves_unrelated_startup_entries_untouched() {
        let directory = Scratch::new();
        let entry = Registration {
            format: Format::DesktopEntry,
            file: directory.0.join("v2t.desktop"),
            executable: PathBuf::from("/opt/v2t"),
        };
        let unrelated = b"[Desktop Entry]\nName=Other\nExec=/bin/other\n";
        fs::write(&entry.file, unrelated).unwrap();
        assert!(entry.set_enabled(true).is_err());
        assert!(entry.set_enabled(false).is_err());
        assert_eq!(fs::read(&entry.file).unwrap(), unrelated);
    }
}
