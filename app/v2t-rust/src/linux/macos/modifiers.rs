use std::{cell::Cell, io, ptr::NonNull, rc::Rc};

use block2::RcBlock;
use objc2::{
    MainThreadMarker,
    rc::{Retained, Weak},
    runtime::AnyObject,
};
use objc2_app_kit::{NSEvent, NSEventMask, NSWindow};

use super::hotkey::Edge;

// Device-side modifier bits defined in IOKit's IOLLEvent.h.
const RIGHT_COMMAND: u64 = 0x10;
const RIGHT_OPTION: u64 = 0x40;
const RIGHT_PAIR: u64 = RIGHT_COMMAND | RIGHT_OPTION;
const RIGHT_COMMAND_KEY: u16 = 0x36;
const RIGHT_OPTION_KEY: u16 = 0x3d;

pub(super) struct ModifierShortcut {
    monitor: Retained<AnyObject>,
    pressed: Rc<Cell<bool>>,
    _main: MainThreadMarker,
}

impl ModifierShortcut {
    pub(super) fn new(
        main: MainThreadMarker,
        window: &NSWindow,
        invoke: impl Fn(Edge) + 'static,
    ) -> io::Result<Self> {
        let pressed = Rc::new(Cell::new(false));
        let held = pressed.clone();
        let window = Weak::new(window);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit provides a live flags-changed event on the main thread.
            let input = unsafe { event.as_ref() };
            if matches!(input.keyCode(), RIGHT_COMMAND_KEY | RIGHT_OPTION_KEY)
                && window.load().is_some_and(|window| window.isKeyWindow())
                && let Some(edge) = transition(&held, input.modifierFlags().bits() as u64)
            {
                invoke(edge);
            }
            event.as_ptr()
        });
        // SAFETY: This local monitor returns the original AppKit event unchanged.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::FlagsChanged,
                &handler,
            )
        }
        .ok_or_else(|| io::Error::other("Could not start the right modifier shortcut."))?;
        Ok(Self {
            monitor,
            pressed,
            _main: main,
        })
    }

    pub(super) fn reset(&self) {
        self.pressed.set(false);
    }
}

impl Drop for ModifierShortcut {
    fn drop(&mut self) {
        // SAFETY: This is the local monitor returned by AppKit on the main thread.
        unsafe { NSEvent::removeMonitor(&self.monitor) };
    }
}

fn transition(pressed: &Cell<bool>, flags: u64) -> Option<Edge> {
    let down = flags & RIGHT_PAIR == RIGHT_PAIR;
    match (pressed.replace(down), down) {
        (false, true) => Some(Edge::Pressed),
        (true, false) => Some(Edge::Released),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn either_right_modifier_can_complete_or_release_the_chord() {
        for first in [RIGHT_COMMAND, RIGHT_OPTION] {
            let pressed = Cell::new(false);
            assert_eq!(transition(&pressed, first), None);
            assert_eq!(transition(&pressed, RIGHT_PAIR), Some(Edge::Pressed));
            assert_eq!(transition(&pressed, RIGHT_PAIR), None);
            assert_eq!(transition(&pressed, first), Some(Edge::Released));
            assert_eq!(transition(&pressed, 0), None);
        }
    }

    #[test]
    fn left_and_aggregate_modifiers_do_not_trigger_the_shortcut() {
        let pressed = Cell::new(false);
        let left_command = 0x08;
        let left_option = 0x20;
        let aggregate_command_option = 0x18_0000;
        for flags in [
            left_command | left_option,
            left_command | RIGHT_OPTION,
            RIGHT_COMMAND | left_option,
            aggregate_command_option,
        ] {
            assert_eq!(transition(&pressed, flags), None);
        }
    }
}
