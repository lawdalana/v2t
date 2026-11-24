use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use objc2::{MainThreadMarker, rc::autoreleasepool};
use objc2_app_kit::{NSAlert, NSApplication};
use objc2_foundation::{NSBundle, NSString};

pub(super) const APP_ID: &str = "com.lawdalana.v2t";
pub(super) const LAUNCHER_ID: &str = "com.lawdalana.v2t.record";

const RECORD_MARKER: &str = "Contents/Resources/Record.v2t-action";

pub(super) fn is_record_request(bundle: &Path, request: &Path) -> bool {
    let Ok(bundle) = bundle.canonicalize() else {
        return false;
    };
    request
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file())
        && request
            .canonicalize()
            .is_ok_and(|path| path == bundle.join(RECORD_MARKER))
}

pub(super) fn run() -> ExitCode {
    let Some(main) = MainThreadMarker::new() else {
        return ExitCode::FAILURE;
    };
    autoreleasepool(|_| match open_recording() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("v2t: {error}");
            let alert = NSAlert::new(main);
            alert.setMessageText(&NSString::from_str("Could not open V2t Record"));
            alert.setInformativeText(&NSString::from_str(&error.to_string()));
            let _ = alert.addButtonWithTitle(&NSString::from_str("OK"));
            // Activate the launcher so its error is visible on macOS 12 and later.
            #[allow(deprecated)]
            NSApplication::sharedApplication(main).activateIgnoringOtherApps(true);
            alert.runModal();
            ExitCode::FAILURE
        }
    })
}

fn open_recording() -> io::Result<()> {
    let launcher = NSBundle::mainBundle();
    let configured_path = launcher
        .objectForInfoDictionaryKey(&NSString::from_str("V2tApplicationPath"))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|path| PathBuf::from(path.to_string()))
        .filter(|path| path.is_absolute());
    let launcher_path = PathBuf::from(launcher.bundlePath().to_string());
    let app = configured_path
        .or_else(|| launcher_path.parent().map(|dir| dir.join("V2t.app")))
        .ok_or_else(missing_app)?;
    let marker = app.join(RECORD_MARKER);
    let is_v2t = app.to_str().is_some_and(|path| {
        NSBundle::bundleWithPath(&NSString::from_str(path))
            .and_then(|bundle| bundle.bundleIdentifier())
            .is_some_and(|id| id.to_string() == APP_ID)
    });
    if !is_v2t || !marker.is_file() {
        return Err(missing_app());
    }

    let status = Command::new("/usr/bin/open")
        .arg("-a")
        .arg(app)
        .arg(marker)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "macOS could not start a recording in V2t.",
        ))
    }
}

fn missing_app() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "Keep V2t Record next to V2t.app or reinstall the launcher.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn accepts_the_bundled_marker_and_rejects_other_documents() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("V2t.app");
        let marker = bundle.join(RECORD_MARKER);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "").unwrap();
        let external = directory.path().join("Record.v2t-action");
        fs::write(&external, "").unwrap();

        assert!(is_record_request(&bundle, &marker));
        assert!(!is_record_request(&bundle, &external));
        assert!(!is_record_request(&bundle, &bundle));
        let alias = directory.path().join("alias.v2t-action");
        symlink(&marker, &alias).unwrap();
        assert!(!is_record_request(&bundle, &alias));
        fs::remove_file(&marker).unwrap();
        assert!(!is_record_request(&bundle, &marker));
        symlink(&external, &marker).unwrap();
        assert!(!is_record_request(&bundle, &external));
        assert!(!is_record_request(&bundle, &marker));
    }
}
