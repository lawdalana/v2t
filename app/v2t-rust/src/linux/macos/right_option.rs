use std::{
    cell::Cell,
    ffi::c_void,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{NonNull, null_mut},
    rc::Rc,
};

use block2::RcBlock;
use objc2::{
    MainThreadMarker,
    rc::{Retained, Weak},
    runtime::AnyObject,
};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType, NSWindow};

use super::hotkey::Edge;

// IOKit's IOLLEvent.h provides device-side modifier flags. Caps Lock is a
// persistent toggle and does not change this shortcut.
const RIGHT_OPTION: u64 = 0x40;
const OTHER_MODIFIERS: u64 = 0x20 // left Option
    | 0x201f // device-side Shift, Control and Command
    | 0x0016_0000 // aggregate Shift, Control and Command
    | 0x00e0_0000; // numeric pad, Help and Fn
const SPACE: u16 = 0x31;
const SESSION_EVENT_TAP: u32 = 1;
const HEAD_INSERT_EVENT_TAP: u32 = 0;
const ACTIVE_EVENT_TAP: u32 = 0;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const TAP_TIMEOUT: u32 = 0xffff_fffe;
const TAP_DISABLED: u32 = 0xffff_ffff;
const KEYCODE_FIELD: u32 = 9;
const AUTOREPEAT_FIELD: u32 = 8;

type NativeRef = *mut c_void;
type TapCallback = unsafe extern "C" fn(NativeRef, u32, NativeRef, NativeRef) -> NativeRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        location: u32,
        placement: u32,
        options: u32,
        mask: u64,
        callback: TapCallback,
        context: NativeRef,
    ) -> NativeRef;
    fn CGEventTapEnable(tap: NativeRef, enabled: bool);
    fn CGEventGetFlags(event: NativeRef) -> u64;
    fn CGEventGetIntegerValueField(event: NativeRef, field: u32) -> i64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: NativeRef;
    static kCFBooleanTrue: NativeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFRunLoopGetMain() -> NativeRef;
    fn CFRunLoopAddSource(run_loop: NativeRef, source: NativeRef, mode: NativeRef);
    fn CFRunLoopPerformBlock(run_loop: NativeRef, mode: NativeRef, block: NativeRef);
    fn CFRunLoopWakeUp(run_loop: NativeRef);
    fn CFMachPortCreateRunLoopSource(
        allocator: NativeRef,
        port: NativeRef,
        order: isize,
    ) -> NativeRef;
    fn CFRunLoopSourceInvalidate(source: NativeRef);
    fn CFMachPortInvalidate(port: NativeRef);
    fn CFRelease(object: NativeRef);
    fn CFDictionaryCreate(
        allocator: NativeRef,
        keys: *const NativeRef,
        values: *const NativeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> NativeRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: NativeRef;
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: NativeRef) -> u8;
}

pub(super) fn has_accessibility() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

pub(super) fn request_accessibility() {
    // The key and value are framework constants with process lifetime.
    unsafe {
        let options = CFDictionaryCreate(
            null_mut(),
            &raw const kAXTrustedCheckOptionPrompt,
            &raw const kCFBooleanTrue,
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        );
        if !options.is_null() {
            AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
        }
    }
}

#[derive(Clone, Copy, Default)]
enum Chord {
    #[default]
    Idle,
    Held,
    WaitingForSpaceUp,
}

#[derive(Clone, Copy)]
enum Input {
    SpaceDown { repeat: bool },
    SpaceUp,
    Other,
}

impl Chord {
    fn interrupt(&mut self) -> Option<Edge> {
        if matches!(self, Self::Held) {
            *self = Self::WaitingForSpaceUp;
            Some(Edge::Released)
        } else {
            None
        }
    }

    fn handle(&mut self, input: Input, flags: u64, mut emit: impl FnMut(Edge)) -> bool {
        // A fresh down after a missing up belongs to a new physical keystroke.
        if (flags & RIGHT_OPTION == 0
            || matches!(input, Input::SpaceUp | Input::SpaceDown { repeat: false }))
            && let Some(edge) = self.interrupt()
        {
            emit(edge);
        }
        match input {
            Input::SpaceDown { repeat: false } => {
                if flags & RIGHT_OPTION != 0 && flags & OTHER_MODIFIERS == 0 {
                    *self = Self::Held;
                    emit(Edge::Pressed);
                    true
                } else {
                    *self = Self::Idle;
                    false
                }
            }
            Input::SpaceDown { repeat: true } => !matches!(self, Self::Idle),
            Input::SpaceUp => !matches!(std::mem::take(self), Self::Idle),
            Input::Other => false,
        }
    }
}

// Core Foundation runs the tap and all deferred work on the AppKit main
// thread. A weak receiver lets already queued blocks expire on reconfiguration.
struct Receiver {
    generation: Cell<u64>,
    invoke: Box<dyn Fn(Edge)>,
}

struct Callback {
    chord: Cell<Chord>,
    receiver: Rc<Receiver>,
    tap: Cell<NativeRef>,
}

impl Callback {
    fn handle(&self, input: Input, flags: u64) -> bool {
        let mut chord = self.chord.get();
        let consumed = chord.handle(input, flags, |edge| self.queue(edge));
        self.chord.set(chord);
        consumed
    }

    fn queue(&self, edge: Edge) {
        let receiver = Rc::downgrade(&self.receiver);
        let generation = self.receiver.generation.get();
        let block = RcBlock::new(move || {
            if let Some(receiver) = receiver.upgrade()
                && receiver.generation.get() == generation
            {
                let _ = catch_unwind(AssertUnwindSafe(|| (receiver.invoke)(edge)));
            }
        });
        // SAFETY: CF copies the block before returning, and dispatches it only
        // on the main run loop. Recording and transcription run outside the tap.
        unsafe {
            let main = CFRunLoopGetMain();
            CFRunLoopPerformBlock(main, kCFRunLoopCommonModes, RcBlock::as_ptr(&block).cast());
            CFRunLoopWakeUp(main);
        }
    }

    fn reset(&self) {
        self.chord.set(Chord::Idle);
        self.receiver
            .generation
            .set(self.receiver.generation.get().wrapping_add(1));
    }
}

enum Monitor {
    Global { port: NativeRef, source: NativeRef },
    Focused(Retained<AnyObject>),
}

pub(super) struct RightOptionShortcut {
    monitor: Monitor,
    callback: Rc<Callback>,
    _main: MainThreadMarker,
}

impl RightOptionShortcut {
    pub(super) fn new(
        main: MainThreadMarker,
        window: &NSWindow,
        invoke: impl Fn(Edge) + 'static,
    ) -> io::Result<Self> {
        let callback = Rc::new(Callback {
            chord: Cell::new(Chord::Idle),
            receiver: Rc::new(Receiver {
                generation: Cell::new(0),
                invoke: Box::new(invoke),
            }),
            tap: Cell::new(null_mut()),
        });
        let monitor = match global_monitor(&callback) {
            Some(monitor) => monitor,
            None => focused_monitor(window, callback.clone())?,
        };
        Ok(Self {
            monitor,
            callback,
            _main: main,
        })
    }

    pub(super) fn is_global(&self) -> bool {
        matches!(self.monitor, Monitor::Global { .. })
    }

    pub(super) fn reset_focused(&self) {
        if !self.is_global() {
            self.callback.reset();
        }
    }
}

impl Drop for RightOptionShortcut {
    fn drop(&mut self) {
        self.callback.reset();
        // SAFETY: Unregister on the same main thread before freeing callback.
        unsafe {
            match &self.monitor {
                Monitor::Global { port, source } => {
                    CGEventTapEnable(*port, false);
                    CFRunLoopSourceInvalidate(*source);
                    CFMachPortInvalidate(*port);
                    CFRelease(*source);
                    CFRelease(*port);
                }
                Monitor::Focused(token) => NSEvent::removeMonitor(token),
            }
        }
    }
}

fn global_monitor(callback: &Rc<Callback>) -> Option<Monitor> {
    if !has_accessibility() {
        return None;
    }
    let mask = (1 << KEY_DOWN) | (1 << KEY_UP) | (1 << FLAGS_CHANGED);
    // SAFETY: The session tap is an active filter, installed at the head of
    // the tap list. Its context stays alive until its main-loop source is removed.
    unsafe {
        let port = CGEventTapCreate(
            SESSION_EVENT_TAP,
            HEAD_INSERT_EVENT_TAP,
            ACTIVE_EVENT_TAP,
            mask,
            tap_event,
            Rc::as_ptr(callback).cast_mut().cast(),
        );
        if port.is_null() {
            return None;
        }
        let source = CFMachPortCreateRunLoopSource(null_mut(), port, 0);
        if source.is_null() {
            CFMachPortInvalidate(port);
            CFRelease(port);
            return None;
        }
        callback.tap.set(port);
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(port, true);
        Some(Monitor::Global { port, source })
    }
}

unsafe extern "C" fn tap_event(
    _: NativeRef,
    kind: u32,
    event: NativeRef,
    context: NativeRef,
) -> NativeRef {
    // SAFETY: CoreGraphics supplies the registered context on the main thread.
    let callback = unsafe { &*context.cast::<Callback>() };
    if matches!(kind, TAP_TIMEOUT | TAP_DISABLED) {
        let mut chord = callback.chord.get();
        if let Some(edge) = chord.interrupt() {
            callback.queue(edge);
        }
        callback.chord.set(chord);
        unsafe { CGEventTapEnable(callback.tap.get(), true) };
        return event;
    }
    if event.is_null() {
        return event;
    }
    // Read only whether this is Space and the modifiers needed by the chord.
    let input = match kind {
        KEY_DOWN | KEY_UP
            if unsafe { CGEventGetIntegerValueField(event, KEYCODE_FIELD) } == i64::from(SPACE) =>
        {
            if kind == KEY_UP {
                Input::SpaceUp
            } else {
                Input::SpaceDown {
                    repeat: unsafe { CGEventGetIntegerValueField(event, AUTOREPEAT_FIELD) } != 0,
                }
            }
        }
        _ => Input::Other,
    };
    if callback.handle(input, unsafe { CGEventGetFlags(event) }) {
        null_mut()
    } else {
        event
    }
}

fn focused_monitor(window: &NSWindow, callback: Rc<Callback>) -> io::Result<Monitor> {
    let window = Weak::new(window);
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| {
        // SAFETY: AppKit supplies a live keyboard event on the main thread.
        let key = unsafe { event.as_ref() };
        if !window.load().is_some_and(|window| {
            window.isKeyWindow()
                && (key.windowNumber() <= 0 || key.windowNumber() == window.windowNumber())
        }) {
            return event.as_ptr();
        }
        let input = match key.r#type() {
            NSEventType::KeyDown if key.keyCode() == SPACE => Input::SpaceDown {
                repeat: key.isARepeat(),
            },
            NSEventType::KeyUp if key.keyCode() == SPACE => Input::SpaceUp,
            _ => Input::Other,
        };
        if callback.handle(input, key.modifierFlags().bits() as u64) {
            null_mut()
        } else {
            event.as_ptr()
        }
    });
    // SAFETY: A local monitor may consume its event by returning null.
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::KeyDown | NSEventMask::KeyUp | NSEventMask::FlagsChanged,
            &handler,
        )
    }
    .map(Monitor::Focused)
    .ok_or_else(|| io::Error::other("Could not monitor Right Option + Space in the v2t window."))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: u64 = 0x0008_0040;

    fn event(chord: &mut Chord, input: Input, flags: u64) -> (bool, Vec<Edge>) {
        let mut edges = vec![];
        let consumed = chord.handle(input, flags, |edge| edges.push(edge));
        (consumed, edges)
    }

    #[test]
    fn releasing_option_first_finishes_once_and_owns_only_the_matching_space() {
        let mut chord = Chord::default();
        let down = Input::SpaceDown { repeat: false };
        let repeat = Input::SpaceDown { repeat: true };
        assert_eq!(event(&mut chord, Input::Other, RIGHT), (false, vec![]));
        assert_eq!(event(&mut chord, down, RIGHT), (true, vec![Edge::Pressed]));
        assert_eq!(event(&mut chord, repeat, RIGHT), (true, vec![]));
        assert_eq!(
            event(&mut chord, Input::Other, 0),
            (false, vec![Edge::Released])
        );
        assert_eq!(event(&mut chord, repeat, 0), (true, vec![]));
        assert_eq!(event(&mut chord, Input::SpaceUp, 0), (true, vec![]));
        assert_eq!(event(&mut chord, down, 0), (false, vec![]));
        assert_eq!(event(&mut chord, Input::SpaceUp, 0), (false, vec![]));
    }

    #[test]
    fn left_option_other_shortcuts_and_preexisting_space_repeats_pass_through() {
        let mut chord = Chord::default();
        let down = Input::SpaceDown { repeat: false };
        for flags in [
            0,
            0x80020,
            0x80000,
            RIGHT | 0x20,
            RIGHT | 0x20000,
            RIGHT | 0x40000,
            RIGHT | 0x100000,
            RIGHT | 0x800000,
        ] {
            assert_eq!(event(&mut chord, down, flags), (false, vec![]));
            assert_eq!(event(&mut chord, Input::SpaceUp, flags), (false, vec![]));
        }
        assert_eq!(
            event(&mut chord, Input::SpaceDown { repeat: true }, RIGHT),
            (false, vec![])
        );
        assert_eq!(event(&mut chord, down, RIGHT), (true, vec![Edge::Pressed]));
        assert_eq!(
            event(&mut chord, Input::SpaceUp, RIGHT),
            (true, vec![Edge::Released])
        );
        assert_eq!(event(&mut chord, Input::Other, 0), (false, vec![]));
    }

    #[test]
    fn interrupted_tap_finishes_the_hold_and_consumes_the_remaining_space_events() {
        let mut chord = Chord::default();
        let down = Input::SpaceDown { repeat: false };
        assert_eq!(event(&mut chord, down, RIGHT), (true, vec![Edge::Pressed]));
        assert_eq!(chord.interrupt(), Some(Edge::Released));
        assert_eq!(chord.interrupt(), None);
        assert_eq!(
            event(&mut chord, Input::SpaceDown { repeat: true }, RIGHT),
            (true, vec![])
        );
        assert_eq!(event(&mut chord, Input::SpaceUp, RIGHT), (true, vec![]));
        assert_eq!(event(&mut chord, down, 0), (false, vec![]));
    }

    #[test]
    fn fresh_space_down_recovers_from_a_missed_key_up() {
        let mut chord = Chord::default();
        let down = Input::SpaceDown { repeat: false };
        assert_eq!(event(&mut chord, down, RIGHT), (true, vec![Edge::Pressed]));
        assert_eq!(
            event(&mut chord, down, RIGHT),
            (true, vec![Edge::Released, Edge::Pressed])
        );
        assert_eq!(event(&mut chord, down, 0), (false, vec![Edge::Released]));
        assert_eq!(event(&mut chord, Input::SpaceUp, 0), (false, vec![]));
    }
}
