use std::{io, path::PathBuf};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSModalResponseCancel, NSModalResponseOK, NSOpenPanel, NSSavePanel};
use objc2_foundation::{NSArray, NSString};
use objc2_uniform_type_identifiers::UTTypeJSON;

pub(super) fn open(main: MainThreadMarker) -> io::Result<Option<PathBuf>> {
    let panel = NSOpenPanel::openPanel(main);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    configure(&panel, "Import");
    selected_file(&panel)
}

pub(super) fn save(main: MainThreadMarker) -> io::Result<Option<PathBuf>> {
    let panel = NSSavePanel::savePanel(main);
    panel.setNameFieldStringValue(&NSString::from_str("v2t-dictionary.json"));
    panel.setAllowsOtherFileTypes(false);
    configure(&panel, "Export");
    selected_file(&panel)
}

fn configure(panel: &NSSavePanel, action: &str) {
    // SAFETY: Uniform Type Identifiers provides this shared system type on macOS 11 and later.
    let json = unsafe { UTTypeJSON };
    panel.setAllowedContentTypes(&NSArray::from_slice(&[json]));
    panel.setPrompt(Some(&NSString::from_str(action)));
}

fn selected_file(panel: &NSSavePanel) -> io::Result<Option<PathBuf>> {
    let response = panel.runModal();
    if response == NSModalResponseCancel {
        return Ok(None);
    }
    if response != NSModalResponseOK {
        return Err(io::Error::other("Could not open the file picker."));
    }
    let path = panel
        .URL()
        .filter(|url| url.isFileURL())
        .and_then(|url| url.path())
        .ok_or_else(|| io::Error::other("Could not read the selected file path."))?;
    Ok(Some(PathBuf::from(path.to_string())))
}
