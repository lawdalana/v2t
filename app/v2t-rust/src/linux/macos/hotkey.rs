use std::{
    cell::Cell,
    ffi::c_void,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{NonNull, null_mut},
    rc::Rc,
};

use objc2::MainThreadMarker;

use super::preferences::Shortcut;

const KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const HOTKEY_ID: u32 = u32::from_be_bytes(*b"hkid");
const SIGNATURE: u32 = u32::from_be_bytes(*b"v2t!");
const PRESSED: u32 = 5;
const RELEASED: u32 = 6;
const NOT_HANDLED: i32 = -9874;
const EXCLUSIVE: u32 = 1;
const COMMAND: u32 = 1 << 8;
const OPTION: u32 = 1 << 11;
const CONTROL: u32 = 1 << 12;
const SHIFT: u32 = 1 << 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Edge {
    Pressed,
    Released,
}

#[repr(C)]
#[derive(Clone, Copy, Eq, PartialEq)]
struct HotkeyId {
    signature: u32,
    id: u32,
}

#[repr(C)]
struct EventType {
    class: u32,
    kind: u32,
}

type EventRef = *mut c_void;
type EventHandler = unsafe extern "C" fn(EventRef, EventRef, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventRef;
    fn GetEventKind(event: EventRef) -> u32;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        kind: u32,
        actual_kind: *mut u32,
        size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
    fn InstallEventHandler(
        target: EventRef,
        handler: EventHandler,
        count: usize,
        events: *const EventType,
        context: *mut c_void,
        handle: *mut EventRef,
    ) -> i32;
    fn RemoveEventHandler(handler: EventRef) -> i32;
    fn RegisterEventHotKey(
        key: u32,
        modifiers: u32,
        id: HotkeyId,
        target: EventRef,
        options: u32,
        hotkey: *mut EventRef,
    ) -> i32;
    fn UnregisterEventHotKey(hotkey: EventRef) -> i32;
}

struct Callback {
    id: Cell<Option<HotkeyId>>,
    pressed: Cell<bool>,
    invoke: Box<dyn Fn(Edge)>,
}

pub(super) struct Hotkey {
    handler: NonNull<c_void>,
    registration: Option<Registration>,
    callback: Rc<Callback>,
    _main: MainThreadMarker,
}

impl Hotkey {
    pub(super) fn new(main: MainThreadMarker, invoke: impl Fn(Edge) + 'static) -> io::Result<Self> {
        let callback = Rc::new(Callback {
            id: Cell::new(None),
            pressed: Cell::new(false),
            invoke: Box::new(invoke),
        });
        let events = [
            EventType {
                class: KEYBOARD,
                kind: PRESSED,
            },
            EventType {
                class: KEYBOARD,
                kind: RELEASED,
            },
        ];
        let mut handler = null_mut();
        // SAFETY: Carbon dispatches these events on the application main thread.
        // The reference-counted context is held until the handler is removed.
        unsafe {
            let target = GetApplicationEventTarget();
            let status = InstallEventHandler(
                target,
                dispatch,
                events.len(),
                events.as_ptr(),
                Rc::as_ptr(&callback).cast_mut().cast(),
                &mut handler,
            );
            let Some(handler) = NonNull::new(handler).filter(|_| status == 0) else {
                return Err(io::Error::other(format!(
                    "Could not install the keyboard shortcut handler ({status})."
                )));
            };
            Ok(Self {
                handler,
                registration: None,
                callback,
                _main: main,
            })
        }
    }

    pub(super) fn current(&self) -> Shortcut {
        self.registration
            .as_ref()
            .map_or(Shortcut::Off, |registration| registration.shortcut)
    }

    pub(super) fn apply(&mut self, registration: Option<Registration>) {
        self.callback
            .id
            .set(registration.as_ref().map(|registration| registration.id));
        self.callback.pressed.set(false);
        self.registration = registration;
    }
}

impl Drop for Hotkey {
    fn drop(&mut self) {
        self.registration = None;
        // SAFETY: The handler belongs to this controller on the main thread.
        unsafe { RemoveEventHandler(self.handler.as_ptr()) };
    }
}

pub(super) struct Registration {
    shortcut: Shortcut,
    id: HotkeyId,
    reference: NonNull<c_void>,
    _main: MainThreadMarker,
}

impl Registration {
    pub(super) fn new(main: MainThreadMarker, shortcut: Shortcut) -> io::Result<Option<Self>> {
        let (key, modifiers) = match shortcut {
            Shortcut::ControlShiftSpace => (0x31, CONTROL | SHIFT),
            Shortcut::CommandShiftSpace => (0x31, COMMAND | SHIFT),
            Shortcut::CommandOptionSpace => (0x31, COMMAND | OPTION),
            Shortcut::ControlF9 => (0x65, CONTROL),
            Shortcut::RightCommandOption | Shortcut::RightOptionSpace | Shortcut::Off => {
                return Ok(None);
            }
        };
        let id = HotkeyId {
            signature: SIGNATURE,
            // Different modifier combinations can share the same physical key.
            id: (modifiers << 8) | key,
        };
        let mut reference = null_mut();
        // SAFETY: The application event target is valid on the main thread.
        let status = unsafe {
            RegisterEventHotKey(
                key,
                modifiers,
                id,
                GetApplicationEventTarget(),
                EXCLUSIVE,
                &mut reference,
            )
        };
        let Some(reference) = NonNull::new(reference).filter(|_| status == 0) else {
            return Err(io::Error::other(format!(
                "Could not register {} ({status}). Choose another shortcut.",
                shortcut.label()
            )));
        };
        Ok(Some(Self {
            shortcut,
            id,
            reference,
            _main: main,
        }))
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: This registration owns the hotkey reference on the main thread.
        unsafe { UnregisterEventHotKey(self.reference.as_ptr()) };
    }
}

unsafe extern "C" fn dispatch(_: EventRef, event: EventRef, context: *mut c_void) -> i32 {
    // SAFETY: Carbon supplies the event and the live Callback registered above.
    let callback = unsafe { &*context.cast::<Callback>() };
    let mut id = HotkeyId {
        signature: 0,
        id: 0,
    };
    let result = unsafe {
        GetEventParameter(
            event,
            DIRECT_OBJECT,
            HOTKEY_ID,
            null_mut(),
            size_of::<HotkeyId>(),
            null_mut(),
            (&mut id as *mut HotkeyId).cast(),
        )
    };
    if result != 0 || Some(id) != callback.id.get() {
        return NOT_HANDLED;
    }
    let edge = match unsafe { GetEventKind(event) } {
        PRESSED => Edge::Pressed,
        RELEASED => Edge::Released,
        _ => return NOT_HANDLED,
    };
    if new_edge(&callback.pressed, edge) {
        let _ = catch_unwind(AssertUnwindSafe(|| (callback.invoke)(edge)));
    }
    0
}

fn new_edge(pressed: &Cell<bool>, edge: Edge) -> bool {
    let down = edge == Edge::Pressed;
    pressed.replace(down) != down
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_press_and_release_even_with_key_repeat() {
        let pressed = Cell::new(false);
        assert!(!new_edge(&pressed, Edge::Released));
        assert!(new_edge(&pressed, Edge::Pressed));
        assert!(!new_edge(&pressed, Edge::Pressed));
        assert!(new_edge(&pressed, Edge::Released));
        assert!(!new_edge(&pressed, Edge::Released));
    }
}
