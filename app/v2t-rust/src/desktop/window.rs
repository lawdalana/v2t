use super::{APPLY, COPY, DICT_LIST, DICT_RETURN, DICTIONARY, EVENTS, EXIT, Event, WAKE, enqueue};
use std::{
    cell::Cell,
    io, mem,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::COLOR_WINDOW,
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                WTSUnRegisterSessionNotification,
            },
        },
        UI::{
            Shell::{
                NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
                NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::*,
        },
    },
    w,
};

const TRAY_EVENT: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
const TIMER_ID: usize = 1;

thread_local! {
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

pub(super) struct Window(pub HWND);
impl Window {
    pub(super) fn create() -> io::Result<Self> {
        let class = w!("v2t-rust-desktop");
        // SAFETY: the module is the current executable, and the class and title are static strings.
        let handle = unsafe {
            let module = GetModuleHandleW(null());
            let class_data = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: module,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_WINDOW + 1) as _,
                lpszClassName: class,
                ..Default::default()
            };
            if RegisterClassW(&class_data) == 0 {
                return Err(io::Error::last_os_error());
            }
            let handle = CreateWindowExW(
                WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW,
                class,
                w!("v2t Settings"),
                WS_CAPTION | WS_SYSMENU,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                606,
                423,
                null_mut(),
                null_mut(),
                module,
                null_mut(),
            );
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            WTSRegisterSessionNotification(handle, NOTIFY_FOR_THIS_SESSION);
            TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));
            SetTimer(handle, TIMER_ID, 2000, None);
            handle
        };
        Ok(Self(handle))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: unregister notifications and close the owned window on this thread.
        unsafe {
            KillTimer(self.0, TIMER_ID);
            WTSUnRegisterSessionNotification(self.0);
            DestroyWindow(self.0);
        }
        let _ = EVENTS.try_with(|slot| slot.borrow_mut().take());
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WTS_SESSION_CHANGE: u32 = 0x02b1;
    let event = match message {
        WM_CLOSE => Some(Event::Hide),
        WM_QUERYENDSESSION => return 1,
        WM_ENDSESSION if wparam != 0 => Some(Event::Exit),
        WM_TIMER if wparam == TIMER_ID => Some(Event::Tick),
        WM_TIMECHANGE | WTS_SESSION_CHANGE => Some(Event::Resume),
        WM_POWERBROADCAST if wparam == PBT_APMRESUMEAUTOMATIC as usize => Some(Event::Resume),
        WM_COMMAND if lparam != 0 => control_command(wparam),
        TRAY_EVENT => match lparam as u32 {
            WM_RBUTTONUP | WM_CONTEXTMENU => Some(Event::TrayMenu),
            WM_LBUTTONDBLCLK => Some(Event::Action(DICT_RETURN)),
            _ => None,
        },
        WAKE => return 0,
        id if id != 0 && TASKBAR_CREATED.try_with(Cell::get).ok() == Some(id) => {
            Some(Event::TaskbarRestart)
        }
        _ => None,
    };
    if let Some(event) = event {
        let _ = EVENTS.try_with(|events| {
            if let Ok(slot) = events.try_borrow()
                && let Some(sender) = slot.as_ref()
            {
                enqueue(sender, window as usize, event);
            }
        });
        return 0;
    }
    // SAFETY: forward all remaining messages from Windows unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn control_command(wparam: WPARAM) -> Option<Event> {
    let id = wparam as u16;
    let notification = ((wparam >> 16) & 0xffff) as u32;
    let accepted = if id == DICT_LIST {
        notification == LBN_SELCHANGE
    } else {
        (APPLY..DICT_LIST).contains(&id) && notification == BN_CLICKED
    };
    accepted.then_some(Event::Action(id))
}

pub(super) struct Tray {
    window: HWND,
    icon: HICON,
    owns_icon: bool,
}

impl Tray {
    pub(super) fn create(window: HWND) -> Self {
        let icon = embedded_icon();
        let (icon, owns_icon) = if icon.is_null() {
            // SAFETY: shared standard icon is valid for any UI thread.
            (unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }, false)
        } else {
            (icon, true)
        };
        let tray = Self {
            window,
            icon,
            owns_icon,
        };
        tray.add();
        tray
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.window,
            uID: TRAY_ID,
            ..Default::default()
        }
    }

    pub(super) fn add(&self) {
        let mut data = self.data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = TRAY_EVENT;
        data.hIcon = self.icon;
        write_label(&mut data.szTip, "v2t");
        // SAFETY: all input fields are initialized and Windows copies the data during the call.
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &data);
        }
    }

    pub(super) fn status(&self, text: &str, notify: bool) {
        let mut data = self.data();
        data.uFlags = NIF_TIP;
        write_label(&mut data.szTip, &format!("v2t · {text}"));
        if notify {
            data.uFlags |= NIF_INFO;
            data.dwInfoFlags = NIIF_INFO;
            write_label(&mut data.szInfoTitle, "v2t");
            write_label(&mut data.szInfo, text);
        }
        // SAFETY: all input fields are initialized and Windows copies the data during the call.
        unsafe {
            Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub(super) fn menu(&self, can_copy: bool) -> usize {
        // SAFETY: all menu labels are static, the popup is owned until TrackPopupMenu returns.
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return 0;
            }
            AppendMenuW(menu, MF_STRING, APPLY as usize, w!("Settings"));
            AppendMenuW(
                menu,
                MF_STRING,
                DICTIONARY as usize,
                w!("Dictionary && AI…"),
            );
            let state = if can_copy { MF_ENABLED } else { MF_GRAYED };
            AppendMenuW(menu, MF_STRING | state, COPY, w!("Copy last transcript"));
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(menu, MF_STRING, EXIT, w!("Exit"));
            let mut point = POINT::default();
            GetCursorPos(&mut point);
            SetForegroundWindow(self.window);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                self.window,
                null(),
            );
            PostMessageW(self.window, WM_NULL, 0, 0);
            DestroyMenu(menu);
            command as usize
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        // SAFETY: the data identifies our tray icon and we own non-shared icon handles.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.data());
            if self.owns_icon {
                DestroyIcon(self.icon);
            }
        }
    }
}

fn write_label(destination: &mut [u16], value: &str) {
    let mut offset = 0;
    for character in value.chars() {
        let mut storage = [0; 2];
        let encoded = character.encode_utf16(&mut storage);
        if offset + encoded.len() >= destination.len() {
            break;
        }
        destination[offset..offset + encoded.len()].copy_from_slice(encoded);
        offset += encoded.len();
    }
}

fn embedded_icon() -> HICON {
    let file = include_bytes!("../../../V2t.Windows/Assets/v2t.ico");
    let read_u32 =
        |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().expect("icon directory field")) as usize;
    let count = u16::from_le_bytes([file[4], file[5]]) as usize;
    // Use the 32px image if available so Windows doesn't scale the largest image to the tray.
    let Some(entry) = file[6..].chunks_exact(16).take(count).min_by_key(|entry| {
        let size = if entry[0] == 0 {
            256i32
        } else {
            i32::from(entry[0])
        };
        (size - 32).abs()
    }) else {
        return null_mut();
    };
    let len = read_u32(&entry[8..12]);
    let start = read_u32(&entry[12..16]);
    let Some(bytes) = file.get(start..start.saturating_add(len)) else {
        return null_mut();
    };
    let icon_data = bytes.to_vec();
    // SAFETY: resource bytes are a validated slice of the embedded ICO and live during the call.
    unsafe {
        CreateIconFromResourceEx(
            icon_data.as_ptr(),
            icon_data.len() as u32,
            1,
            0x0003_0000,
            32,
            32,
            LR_DEFAULTCOLOR,
        )
    }
}
