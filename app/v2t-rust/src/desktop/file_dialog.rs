use crate::windows_startup::Apartment;
use std::{ffi::OsString, io, os::windows::ffi::OsStringExt, path::PathBuf};
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND},
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
        UI::Shell::{
            Common::COMDLG_FILTERSPEC, FOS_DONTADDTORECENT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
            FOS_OVERWRITEPROMPT, FileOpenDialog, FileSaveDialog, IFileDialog, SIGDN_FILESYSPATH,
        },
    },
    core::{HRESULT, w},
};

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Import,
    Export,
}

pub(super) fn choose_dictionary(
    owner: windows_sys::Win32::Foundation::HWND,
    mode: Mode,
) -> io::Result<Option<PathBuf>> {
    let _apartment = Apartment::new()?;
    let dialog = configure(mode)?;
    // SAFETY: the owner is a live window on this thread and the apartment owns the dialog.
    match unsafe { dialog.Show(Some(HWND(owner))) } {
        Ok(()) => {}
        Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
        Err(error) => return Err(error.into()),
    }

    // SAFETY: Show succeeded, so the selected filesystem item has a path. The returned
    // null-terminated UTF-16 buffer belongs to this call and CoTaskMemFree releases it.
    unsafe {
        let item = dialog.GetResult()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let result = PathBuf::from(OsString::from_wide(path.as_wide()));
        CoTaskMemFree(Some(path.0.cast()));
        Ok(Some(result))
    }
}

fn configure(mode: Mode) -> windows::core::Result<IFileDialog> {
    let (class, title, label, mode_option) = match mode {
        Mode::Import => (
            FileOpenDialog,
            w!("Import dictionary"),
            w!("Import"),
            FOS_FILEMUSTEXIST,
        ),
        Mode::Export => (
            FileSaveDialog,
            w!("Export dictionary"),
            w!("Export"),
            FOS_OVERWRITEPROMPT,
        ),
    };
    // SAFETY: The calling thread initialized COM. The requested shell classes are
    // in-process, all strings are static, and SetFileTypes copies the filter.
    unsafe {
        let dialog: IFileDialog = CoCreateInstance(&class, None, CLSCTX_INPROC_SERVER)?;
        let options =
            dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_DONTADDTORECENT | mode_option;
        dialog.SetOptions(options)?;
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
            pszName: w!("Dictionary JSON (*.json)"),
            pszSpec: w!("*.json"),
        }])?;
        dialog.SetTitle(title)?;
        dialog.SetOkButtonLabel(label)?;
        dialog.SetDefaultExtension(w!("json"))?;
        if matches!(mode, Mode::Export) {
            dialog.SetFileName(w!("v2t-dictionary.json"))?;
        }
        Ok(dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_requires_a_file_and_export_confirms_overwrite() {
        let _apartment = Apartment::new().unwrap();
        let import = configure(Mode::Import).unwrap();
        let export = configure(Mode::Export).unwrap();
        // SAFETY: both dialogs are live on this initialized apartment.
        let (import, export) =
            unsafe { (import.GetOptions().unwrap(), export.GetOptions().unwrap()) };
        assert!(import.contains(FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST));
        assert!(export.contains(FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT));
    }
}
