use std::{
    cell::RefCell,
    io,
    marker::PhantomData,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{copy_nonoverlapping, null, null_mut},
    rc::Rc,
};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        LibraryLoader::GetModuleHandleW,
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            KEYEVENTF_UNICODE, SendInput,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo,
            GetWindowThreadProcessId, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
            LLKHF_INJECTED, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
            WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    },
};

use crate::shortcut::{Action, Binding, Shortcut};

// Use integer representations so the handles can be passed through an application queue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FocusTarget {
    pub window: isize,
    pub control: isize,
}

impl FocusTarget {
    pub fn matches(self, current: Self) -> bool {
        self.window != 0
            && self.window == current.window
            && (self.control == 0 || self.control == current.control)
    }
}

struct HookState {
    shortcut: Shortcut,
    try_pressed: Box<dyn FnMut(FocusTarget) -> bool>,
    released: Box<dyn FnMut(bool)>,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// Owner of the low-level keyboard hook for the calling Windows message-loop thread.
pub struct Keyboard {
    hook: HHOOK,
    _owner_thread: PhantomData<Rc<()>>,
}

impl Keyboard {
    pub fn install(
        binding: Binding,
        try_pressed: impl FnMut(FocusTarget) -> bool + 'static,
        released: impl FnMut(bool) + 'static,
    ) -> io::Result<Self> {
        validate_binding(binding)?;
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "A dictation shortcut is already installed on this thread.",
                ));
            }
            *state = Some(HookState {
                shortcut: Shortcut::new(binding),
                try_pressed: Box::new(try_pressed),
                released: Box::new(released),
            });
            Ok(())
        })?;

        // SAFETY: WH_KEYBOARD_LL uses this static callback on the installing thread.
        // Its Rust state belongs to that thread and is discarded only after unhooking.
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(observe_keyboard),
                GetModuleHandleW(null()),
                0,
            )
        };
        if hook.is_null() {
            let error = io::Error::last_os_error();
            STATE.with(|state| state.borrow_mut().take());
            return Err(error);
        }

        Ok(Self {
            hook,
            _owner_thread: PhantomData,
        })
    }

    pub fn set_binding(&self, binding: Binding) -> io::Result<()> {
        validate_binding(binding)?;
        with_state_mut(|state| {
            if state.shortcut.set_binding(binding) == Action::Cancel {
                (state.released)(true);
            }
        });
        Ok(())
    }

    pub fn reset(&self) {
        with_state_mut(|state| {
            if state.shortcut.reset() == Action::Cancel {
                (state.released)(true);
            }
        });
    }

    pub fn keys_released(&self) -> bool {
        STATE.with(|state| {
            state
                .try_borrow()
                .ok()
                .and_then(|state| state.as_ref().map(|s| s.shortcut.keys_released(&is_down)))
                .unwrap_or(false)
        })
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        // SAFETY: self is thread-affine and exclusively owns this HHOOK. Its callback
        // references a static function and a TLS slot, neither a freed Rust closure nor self.
        unsafe { UnhookWindowsHookEx(self.hook) };
        let _ = STATE.try_with(|state| {
            if let Ok(mut state) = state.try_borrow_mut() {
                state.take();
            }
        });
    }
}

fn validate_binding(binding: Binding) -> io::Result<()> {
    if !binding.is_valid() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a valid dictation shortcut.",
        ));
    }
    Ok(())
}

fn with_state_mut<R>(update: impl FnOnce(&mut HookState) -> R) -> Option<R> {
    STATE
        .try_with(|state| {
            state
                .try_borrow_mut()
                .ok()
                .and_then(|mut state| state.as_mut().map(update))
        })
        .ok()
        .flatten()
}

unsafe extern "system" fn observe_keyboard(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    let action = message as u32;
    if code == HC_ACTION as i32
        && data != 0
        && matches!(action, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP)
    {
        // SAFETY: for HC_ACTION Windows supplies a KBDLLHOOKSTRUCT valid during this callback.
        let event = unsafe { (data as *const KBDLLHOOKSTRUCT).read() };
        if event.flags & LLKHF_INJECTED == 0 {
            let consumed = catch_unwind(AssertUnwindSafe(|| {
                with_state_mut(|state| {
                    let key = sided_key(event.vkCode, event.scanCode, event.flags);
                    let down = matches!(action, WM_KEYDOWN | WM_SYSKEYDOWN);
                    let (action, consume) =
                        state.shortcut.observe(key, down, Some(&is_down), || {
                            (state.try_pressed)(capture_focus())
                        });
                    match action {
                        Action::Cancel => (state.released)(true),
                        Action::Finish => (state.released)(false),
                        _ => {}
                    }
                    consume
                })
                .unwrap_or(false)
            }))
            .unwrap_or(false);
            if consumed {
                return 1;
            }
        }
    }

    // SAFETY: unhandled hook data is forwarded unchanged. The handle parameter is ignored.
    unsafe { CallNextHookEx(null_mut(), code, message, data) }
}

fn sided_key(key: u32, scan_code: u32, flags: u32) -> u32 {
    match key {
        0x10 => {
            if scan_code == 0x36 {
                0xa1
            } else {
                0xa0
            }
        }
        0x11 => {
            if flags & LLKHF_EXTENDED != 0 {
                0xa3
            } else {
                0xa2
            }
        }
        0x12 => {
            if flags & LLKHF_EXTENDED != 0 {
                0xa5
            } else {
                0xa4
            }
        }
        _ => key,
    }
}

fn is_down(key: u32) -> bool {
    // SAFETY: querying a Windows virtual key neither stores a pointer nor changes key state.
    unsafe { GetAsyncKeyState(key as i32) < 0 }
}

pub fn capture_focus() -> FocusTarget {
    // SAFETY: windows and thread IDs are observed, not owned. info lives through the
    // synchronous call and its size matches Windows' structure.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return FocusTarget::default();
        }
        let thread = GetWindowThreadProcessId(window, null_mut());
        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let control = if thread != 0 && GetGUIThreadInfo(thread, &mut info) != 0 {
            info.hwndFocus
        } else {
            null_mut()
        };
        FocusTarget {
            window: window as isize,
            control: control as isize,
        }
    }
}

pub fn try_insert(target: FocusTarget, text: &str) -> bool {
    if text.is_empty() || text.contains('\0') {
        return false;
    }
    let events = unicode_inputs(text);
    let Ok(count) = events.len().try_into() else {
        return false;
    };
    if [0x10, 0x11, 0x12, 0x5b, 0x5c].into_iter().any(is_down) || !target.matches(capture_focus()) {
        return false;
    }
    // SAFETY: events precisely matches the INPUT layout and remains live until SendInput returns.
    (unsafe { SendInput(count, events.as_ptr(), size_of::<INPUT>() as i32) }) == count
}

fn unicode_inputs(text: &str) -> Vec<INPUT> {
    text.encode_utf16()
        .flat_map(|unit| {
            [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP].map(|flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wScan: unit,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            })
        })
        .collect()
}

struct Clipboard;

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: the only constructor path first opens the clipboard on this thread.
        unsafe { CloseClipboard() };
    }
}

struct GlobalText(HGLOBAL);

impl Drop for GlobalText {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this is an unlocked GlobalAlloc handle that we have not transferred.
            unsafe { GlobalFree(self.0) };
        }
    }
}

pub fn copy_text(owner: isize, text: &str) -> io::Result<()> {
    const CF_UNICODETEXT: u32 = 13;
    if owner == 0 || text.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid clipboard text or owner.",
        ));
    }
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: allocate enough movable memory for all UTF-16 units, including the terminator.
    // Once SetClipboardData succeeds Windows owns the handle, otherwise RAII frees it.
    unsafe {
        let buffer = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(utf16.as_slice()));
        if buffer.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = GlobalText(buffer);
        let output = GlobalLock(buffer.0).cast::<u16>();
        if output.is_null() {
            return Err(io::Error::last_os_error());
        }
        copy_nonoverlapping(utf16.as_ptr(), output, utf16.len());
        GlobalUnlock(buffer.0);

        if OpenClipboard(owner as HWND) == 0 {
            return Err(io::Error::last_os_error());
        }
        let _clipboard = Clipboard;
        if EmptyClipboard() == 0 || SetClipboardData(CF_UNICODETEXT, buffer.0).is_null() {
            return Err(io::Error::last_os_error());
        }
        buffer.0 = null_mut();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_requires_original_window_and_known_control() {
        let original = FocusTarget {
            window: 1,
            control: 2,
        };
        assert!(original.matches(original));
        assert!(!original.matches(FocusTarget {
            window: 1,
            control: 3
        }));
        assert!(!original.matches(FocusTarget {
            window: 4,
            control: 2
        }));
        assert!(
            FocusTarget {
                window: 1,
                control: 0
            }
            .matches(original)
        );
        assert!(!FocusTarget::default().matches(FocusTarget::default()));
    }

    #[test]
    fn sends_thai_and_surrogates_as_ordered_utf16_events() {
        let text = "ท🙂";
        let events = unicode_inputs(text);
        assert_eq!(events.len(), 6);
        for (pair, unit) in events.chunks_exact(2).zip(text.encode_utf16()) {
            let [down, up] = pair else { unreachable!() };
            assert_eq!(down.r#type, INPUT_KEYBOARD);
            assert_eq!(up.r#type, INPUT_KEYBOARD);
            // SAFETY: unicode_inputs initializes the keyboard variant of every event.
            let (down, up) = unsafe { (down.Anonymous.ki, up.Anonymous.ki) };
            assert_eq!((down.wScan, up.wScan), (unit, unit));
            assert_eq!(down.dwFlags, KEYEVENTF_UNICODE);
            assert_eq!(up.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        }
    }

    #[test]
    fn recognizes_both_sides_of_modifier_keys() {
        assert_eq!(sided_key(0x10, 0x36, 0), 0xa1);
        assert_eq!(sided_key(0x10, 0x2a, 0), 0xa0);
        assert_eq!(sided_key(0x11, 0, LLKHF_EXTENDED), 0xa3);
        assert_eq!(sided_key(0x12, 0, LLKHF_EXTENDED), 0xa5);
    }
}
