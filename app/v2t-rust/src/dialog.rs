use crate::{
    config::{Settings, invalid},
    inference::SpeechProvider,
    shortcut::Binding,
    text::{Corrections, Entry},
};
use std::{io, ptr::null_mut};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject},
        System::{LibraryLoader::GetModuleHandleW, SystemServices::SS_LEFT},
        UI::{Controls::EM_SETLIMITTEXT, WindowsAndMessaging::*},
    },
    core::PCWSTR,
    w,
};
use zeroize::Zeroizing;

pub const APPLY: u16 = 100;
pub const DICTIONARY: u16 = 101;
pub const INSTALL: u16 = 102;
pub const DICT_INSTALL: u16 = 103;
pub const DICT_SAVE: u16 = 104;
pub const DICT_RETURN: u16 = 105;
pub const DICT_ADD: u16 = 106;
pub const DICT_UPDATE: u16 = 107;
pub const DICT_REMOVE: u16 = 108;
pub const DICT_LIST: u16 = 109;
pub const START_AT_LOGIN: u16 = 110;

pub struct Form {
    pub window: HWND,
    settings_controls: Vec<HWND>,
    dictionary_controls: Vec<HWND>,
    provider: HWND,
    language: HWND,
    openai: HWND,
    openrouter: HWND,
    backup: HWND,
    start_at_login: HWND,
    shortcut: HWND,
    status: HWND,
    speech_install: HWND,
    correction_install: HWND,
    ai: HWND,
    rows: HWND,
    spoken: HWND,
    replacement: HWND,
    edit_entries: Vec<Entry>,
}

impl Form {
    pub fn create(window: HWND) -> io::Result<Self> {
        let mut settings = Layout::new(window);
        settings.label("Speech provider", 24, 22, 140)?;
        let provider = settings.control(
            w!("COMBOBOX"),
            "",
            0,
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            (180, 18, 384, 220),
        )?;
        for name in [
            "Local · Whisper Turbo",
            "OpenAI · GPT Transcribe",
            "OpenRouter · Whisper Turbo",
        ] {
            add_item(provider, CB_ADDSTRING, name)?;
        }
        settings.label("Dictation language", 24, 65, 145)?;
        let language = settings.control(
            w!("COMBOBOX"),
            "",
            0,
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            (180, 61, 384, 220),
        )?;
        for label in [
            "Thai + English (mixed)",
            "Detect automatically (all languages)",
            "Thai",
            "English",
        ] {
            add_item(language, CB_ADDSTRING, label)?;
        }
        settings.label("OpenAI API key", 24, 106, 145)?;
        let openai = settings.edit(180, 101, 384, 512, true)?;
        settings.label("OpenRouter API key", 24, 147, 149)?;
        let openrouter = settings.edit(180, 142, 384, 512, true)?;
        let backup = settings.control(
            w!("BUTTON"),
            "Use MAI-Transcribe 2 as ZDR backup",
            0,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            (180, 182, 384, 26),
        )?;
        settings.label("Hold to dictate", 24, 229, 145)?;
        let shortcut = settings.edit(180, 224, 180, 48, false)?;
        settings.label("Ctrl+Shift or Ctrl+F9", 372, 229, 192)?;
        let start_at_login = settings.control(
            w!("BUTTON"),
            "Start V2T At Startup",
            START_AT_LOGIN,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            (180, 252, 384, 24),
        )?;
        let speech_install =
            settings.button("Set up local speech model", INSTALL, (24, 282, 190, 32))?;
        settings.button("Dictionary && AI…", DICTIONARY, (224, 282, 165, 32))?;
        settings.button("Save settings", APPLY, (414, 282, 150, 32))?;

        let mut dict = Layout::new(window);
        let ai = dict.control(
            w!("BUTTON"),
            "Use local AI to correct dictation",
            0,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            (24, 20, 310, 25),
        )?;
        let correction_install =
            dict.button("Set up local AI", DICT_INSTALL, (414, 16, 150, 32))?;
        let rows = dict.control(
            w!("LISTBOX"),
            "",
            DICT_LIST,
            WS_BORDER | WS_VSCROLL | WS_TABSTOP | LBS_NOTIFY as u32,
            (24, 58, 540, 148),
        )?;
        dict.label("Spoken text", 24, 216, 260)?;
        dict.label("Replacement", 304, 216, 260)?;
        let spoken = dict.edit(24, 237, 260, 160, false)?;
        let replacement = dict.edit(304, 237, 260, 160, false)?;
        dict.button("Add", DICT_ADD, (24, 277, 78, 31))?;
        dict.button("Update", DICT_UPDATE, (108, 277, 78, 31))?;
        dict.button("Remove", DICT_REMOVE, (192, 277, 78, 31))?;
        dict.button("Back", DICT_RETURN, (344, 277, 77, 31))?;
        dict.button("Save dictionary", DICT_SAVE, (429, 277, 135, 31))?;

        let status =
            Layout::new(window).control(w!("STATIC"), "", 0, SS_LEFT, (24, 328, 540, 48))?;
        set_visible(&dict.handles, false);
        Ok(Self {
            window,
            settings_controls: settings.handles,
            dictionary_controls: dict.handles,
            provider,
            language,
            openai,
            openrouter,
            backup,
            start_at_login,
            shortcut,
            status,
            speech_install,
            correction_install,
            ai,
            rows,
            spoken,
            replacement,
            edit_entries: Vec::new(),
        })
    }

    pub fn show_settings(&self, settings: &Settings) {
        self.fill_settings(settings);
        self.show();
    }

    pub(crate) fn fill_settings(&self, settings: &Settings) {
        send(self.provider, CB_SETCURSEL, settings.provider as usize, 0);
        let language = match settings.language.as_deref() {
            Some("th+en") => 0,
            Some("th") => 2,
            Some("en") => 3,
            _ => 1,
        };
        send(self.language, CB_SETCURSEL, language, 0);
        set_secret(self.openai, &settings.open_ai_key);
        set_secret(self.openrouter, &settings.open_router_key);
        check(self.backup, settings.enable_mai_backup);
        caption(self.shortcut, &settings.shortcut.label());
        caption(self.window, "v2t Settings");
        set_visible(&self.dictionary_controls, false);
        set_visible(&self.settings_controls, true);
    }

    pub fn show_dictionary(&mut self, corrections: &Corrections) {
        self.edit_entries = corrections.entries.clone();
        self.refresh_rows(None);
        check(self.ai, corrections.enable_ai);
        caption(self.window, "v2t Dictionary & AI");
        set_visible(&self.settings_controls, false);
        set_visible(&self.dictionary_controls, true);
        self.show();
    }

    fn show(&self) {
        // SAFETY: the form window is live on the owning UI thread.
        unsafe {
            ShowWindow(self.window, SW_SHOWNORMAL);
            SetForegroundWindow(self.window);
        }
    }

    pub fn clear_secret_controls(&self) {
        set_secret(self.openai, "");
        set_secret(self.openrouter, "");
    }

    pub fn status(&self, message: &str) {
        caption(self.status, message);
    }

    pub fn set_start_at_login(&self, enabled: Option<bool>) {
        check(self.start_at_login, enabled.unwrap_or_default());
        // SAFETY: this button belongs to the live form on its owning thread.
        unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                self.start_at_login,
                i32::from(enabled.is_some()),
            );
        }
    }

    pub fn starts_at_login(&self) -> bool {
        checked(self.start_at_login)
    }

    pub fn installation_buttons(&self, installing: bool, speech: bool, correction: bool) {
        // SAFETY: control handles belong to the live form.
        unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                self.speech_install,
                i32::from(!installing && !speech),
            );
            windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                self.correction_install,
                i32::from(!installing && !correction),
            );
        }
        caption(
            self.speech_install,
            if speech {
                "Speech model installed"
            } else {
                "Set up local speech model"
            },
        );
        caption(
            self.correction_install,
            if correction {
                "AI model installed"
            } else {
                "Set up local AI"
            },
        );
    }

    pub fn settings(&self) -> io::Result<Settings> {
        let provider = match send(self.provider, CB_GETCURSEL, 0, 0) {
            0 => SpeechProvider::Local,
            1 => SpeechProvider::OpenAi,
            2 => SpeechProvider::OpenRouter,
            _ => return Err(invalid("Choose a speech provider.")),
        };
        let language = self.language()?;
        let binding = read_text(self.shortcut, 48)?;
        let shortcut = Binding::parse(&binding)
            .map_err(|_| invalid("Enter a shortcut such as Ctrl+Shift or Ctrl+F9."))?;
        Ok(Settings {
            provider,
            language,
            shortcut,
            open_ai_key: Zeroizing::new(read_secret(self.openai)?.trim().into()),
            open_router_key: Zeroizing::new(read_secret(self.openrouter)?.trim().into()),
            enable_mai_backup: checked(self.backup),
        })
    }

    pub fn language(&self) -> io::Result<Option<String>> {
        match send(self.language, CB_GETCURSEL, 0, 0) {
            0 => Ok(Some("th+en".into())),
            1 => Ok(None),
            2 => Ok(Some("th".into())),
            3 => Ok(Some("en".into())),
            _ => Err(invalid("Choose a dictation language.")),
        }
    }

    pub fn corrections(&self) -> Corrections {
        Corrections {
            enable_ai: checked(self.ai),
            entries: self.edit_entries.clone(),
        }
    }

    pub fn row_selected(&self) {
        if let Some(entry) = self.selection().and_then(|row| self.edit_entries.get(row)) {
            caption(self.spoken, &entry.spoken);
            caption(self.replacement, &entry.replacement);
        }
    }

    pub fn edit_dictionary(&mut self, command: u16) -> io::Result<()> {
        let selected = self.selection();
        let next_index = match command {
            DICT_ADD => {
                if self.edit_entries.len() >= 300 {
                    return Err(invalid("The dictionary can have up to 300 entries."));
                }
                let entry = self.entry()?;
                self.edit_entries.push(entry);
                Some(self.edit_entries.len() - 1)
            }
            DICT_UPDATE => {
                let index = selected.ok_or_else(|| invalid("Choose a dictionary entry first."))?;
                let entry = self.entry()?;
                self.edit_entries[index] = entry;
                Some(index)
            }
            DICT_REMOVE => {
                let index = selected.ok_or_else(|| invalid("Choose a dictionary entry first."))?;
                self.edit_entries.remove(index);
                None
            }
            _ => return Ok(()),
        };
        self.refresh_rows(next_index);
        Ok(())
    }

    fn entry(&self) -> io::Result<Entry> {
        let spoken = read_text(self.spoken, 160)?.trim().to_owned();
        let replacement = read_text(self.replacement, 160)?.trim().to_owned();
        if spoken.is_empty() || replacement.is_empty() {
            return Err(invalid("Enter both spoken text and its replacement."));
        }
        Ok(Entry {
            spoken,
            replacement,
        })
    }

    fn selection(&self) -> Option<usize> {
        usize::try_from(send(self.rows, LB_GETCURSEL, 0, 0))
            .ok()
            .filter(|index| *index < self.edit_entries.len())
    }

    fn refresh_rows(&self, select: Option<usize>) {
        send(self.rows, LB_RESETCONTENT, 0, 0);
        for entry in &self.edit_entries {
            let _ = add_item(
                self.rows,
                LB_ADDSTRING,
                &format!("{}  →  {}", entry.spoken, entry.replacement),
            );
        }
        caption(self.spoken, "");
        caption(self.replacement, "");
        if let Some(index) = select {
            send(self.rows, LB_SETCURSEL, index, 0);
            self.row_selected();
        }
    }
}

struct Layout {
    parent: HWND,
    handles: Vec<HWND>,
}
impl Layout {
    fn new(parent: HWND) -> Self {
        Self {
            parent,
            handles: Vec::new(),
        }
    }

    fn control(
        &mut self,
        class: PCWSTR,
        label: &str,
        id: u16,
        style: u32,
        rect: (i32, i32, i32, i32),
    ) -> io::Result<HWND> {
        let label = wide(label);
        // SAFETY: label buffer is NUL-terminated and Windows copies it during CreateWindowExW.
        let control = unsafe {
            CreateWindowExW(
                0,
                class,
                label.as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                rect.0,
                rect.1,
                rect.2,
                rect.3,
                self.parent,
                id as usize as HMENU,
                GetModuleHandleW(null_mut()),
                null_mut(),
            )
        };
        if control.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: DEFAULT_GUI_FONT is a shared stock object; the control does not take ownership.
        let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
        send(control, WM_SETFONT, font as WPARAM, 1);
        self.handles.push(control);
        Ok(control)
    }

    fn label(&mut self, text: &str, x: i32, y: i32, width: i32) -> io::Result<HWND> {
        self.control(w!("STATIC"), text, 0, SS_LEFT, (x, y, width, 22))
    }

    fn edit(&mut self, x: i32, y: i32, width: i32, limit: usize, secret: bool) -> io::Result<HWND> {
        let style = WS_TABSTOP
            | WS_BORDER
            | ES_AUTOHSCROLL as u32
            | if secret { ES_PASSWORD as u32 } else { 0 };
        let control = self.control(w!("EDIT"), "", 0, style, (x, y, width, 24))?;
        send(control, EM_SETLIMITTEXT, limit, 0);
        Ok(control)
    }

    fn button(&mut self, label: &str, id: u16, rect: (i32, i32, i32, i32)) -> io::Result<HWND> {
        self.control(w!("BUTTON"), label, id, WS_TABSTOP, rect)
    }
}

fn set_visible(controls: &[HWND], visible: bool) {
    for control in controls {
        // SAFETY: control handles are children of the live form.
        unsafe {
            ShowWindow(*control, if visible { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn send(control: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    // SAFETY: calls use live UI-thread handles. For string messages the borrowed data remains live until return.
    unsafe { SendMessageW(control, message, wparam, lparam) }
}

fn add_item(control: HWND, message: u32, label: &str) -> io::Result<()> {
    let value = wide(label);
    if send(control, message, 0, value.as_ptr() as LPARAM) < 0 {
        return Err(io::Error::other("Could not add an item to v2t Settings."));
    }
    Ok(())
}

fn check(control: HWND, checked: bool) {
    send(control, BM_SETCHECK, usize::from(checked), 0);
}
fn checked(control: HWND) -> bool {
    send(control, BM_GETCHECK, 0, 0) == 1
}

pub fn caption(control: HWND, value: &str) {
    let value = wide(value);
    // SAFETY: SetWindowTextW copies a live NUL-terminated string during the call.
    unsafe {
        SetWindowTextW(control, value.as_ptr());
    }
}

fn set_secret(control: HWND, value: &str) {
    let wide = Zeroizing::new(wide(value));
    // SAFETY: the edit control copies the buffer during the call.
    unsafe {
        SetWindowTextW(control, wide.as_ptr());
    }
}

fn read_wide(control: HWND, limit: usize) -> io::Result<Zeroizing<Vec<u16>>> {
    let mut value = Zeroizing::new(vec![0u16; limit + 1]);
    // SAFETY: the buffer contains limit+1 writable UTF-16 units.
    let len = unsafe { GetWindowTextW(control, value.as_mut_ptr(), (limit + 1) as i32) };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    value.truncate(len as usize);
    Ok(value)
}

fn read_secret(control: HWND) -> io::Result<Zeroizing<String>> {
    let value = read_wide(control, 512)?;
    String::from_utf16(&value)
        .map(Zeroizing::new)
        .map_err(|_| invalid("Invalid API key text."))
}

fn read_text(control: HWND, limit: usize) -> io::Result<String> {
    let buffer = read_wide(control, limit)?;
    String::from_utf16(&buffer).map_err(|_| invalid("Invalid text."))
}
