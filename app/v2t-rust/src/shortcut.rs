use serde::{Deserialize, Serialize};

const CONTROL: u32 = 1;
const SHIFT: u32 = 2;
const ALT: u32 = 4;
const SUPPORTED: u32 = CONTROL | SHIFT | ALT;
const SPACE: u32 = 0x20;
const F1: u32 = 0x70;
const F12: u32 = 0x7b;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Binding {
    pub modifiers: u32,
    pub key: u32,
}

impl Default for Binding {
    fn default() -> Self {
        Self {
            modifiers: CONTROL | SHIFT,
            key: 0,
        }
    }
}

impl Binding {
    pub fn is_valid(&self) -> bool {
        self.modifiers & !SUPPORTED == 0
            && match self.key {
                0 => self.modifiers.count_ones() >= 2,
                F1..=F12 => true,
                SPACE | 0x30..=0x39 | 0x41..=0x5a => self.modifiers != 0,
                _ => false,
            }
    }

    pub fn label(self) -> String {
        if !self.is_valid() {
            return String::new();
        }
        let mut parts = Vec::with_capacity(4);
        for (mask, name) in [(CONTROL, "Ctrl"), (SHIFT, "Shift"), (ALT, "Alt")] {
            if self.has(mask) {
                parts.push(name.to_owned());
            }
        }
        let main = match self.key {
            0 => None,
            SPACE => Some("Space".to_owned()),
            F1..=F12 => Some(format!("F{}", self.key - F1 + 1)),
            key => char::from_u32(key).map(|character| character.to_string()),
        };
        parts.extend(main);
        parts.join("+")
    }

    pub fn parse(value: &str) -> Result<Self, &'static str> {
        const INVALID: &str =
            "Choose at least two modifiers, a function key, or a modifier plus A-Z, 0-9 or Space.";
        let mut binding = Self {
            modifiers: 0,
            key: 0,
        };
        for part in value.split('+') {
            let part = part.trim().to_ascii_uppercase();
            let modifier = match part.as_str() {
                "CTRL" | "CONTROL" => CONTROL,
                "SHIFT" => SHIFT,
                "ALT" => ALT,
                _ => 0,
            };
            if modifier != 0 && !binding.has(modifier) && binding.key == 0 {
                binding.modifiers |= modifier;
                continue;
            }
            if binding.key != 0 {
                return Err(INVALID);
            }
            binding.key = match part.as_str() {
                "SPACE" => SPACE,
                _ => match part.as_bytes() {
                    [character @ (b'A'..=b'Z' | b'0'..=b'9')] => u32::from(*character),
                    [b'F', ..] => match part[1..].parse::<u32>() {
                        Ok(number @ 1..=12) => F1 + number - 1,
                        _ => return Err(INVALID),
                    },
                    _ => return Err(INVALID),
                },
            };
        }
        binding.is_valid().then_some(binding).ok_or(INVALID)
    }

    fn has(self, modifier: u32) -> bool {
        self.modifiers & modifier != 0
    }

    #[cfg(any(windows, test))]
    fn is_chord_key(self, key: u32) -> bool {
        (self.key != 0 && self.key == key) || self.has(modifier_for(key))
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    None,
    Start,
    Finish,
    Cancel,
}

#[cfg(any(windows, test))]
pub(crate) struct Shortcut {
    binding: Binding,
    held: Vec<u32>,
    suppressed: u32,
    recording: bool,
    blocked: bool,
}

#[cfg(any(windows, test))]
impl Shortcut {
    pub(crate) fn new(binding: Binding) -> Self {
        Self {
            binding,
            held: Vec::with_capacity(8),
            suppressed: 0,
            recording: false,
            blocked: false,
        }
    }

    pub(crate) fn observe(
        &mut self,
        key: u32,
        down: bool,
        is_down: Option<&dyn Fn(u32) -> bool>,
        try_pressed: impl FnOnce() -> bool,
    ) -> (Action, bool) {
        let consume = key != 0 && key == self.suppressed;
        if consume && !down {
            self.suppressed = 0;
        }

        let binding = self.binding;
        let chord_key = binding.is_chord_key(key);
        if down
            && chord_key
            && let Some(is_down) = is_down
        {
            // A swallowed main key might be absent from the OS's async key state.
            self.held
                .retain(|held| *held == self.suppressed || is_down(*held));
            if !self.held.iter().any(|held| binding.is_chord_key(*held)) {
                self.blocked = false;
                if self.recording {
                    self.held.push(key);
                    return (self.end(Action::Cancel), consume);
                }
            }
        }

        // After a reset a previously held main key must not quietly start a new chord.
        if down && key != 0 && key == binding.key && !consume && is_down.is_some_and(|get| get(key))
        {
            self.update_held(key, true);
            return (Action::None, false);
        }
        if !self.update_held(key, down) {
            return (Action::None, consume);
        }

        if !self.held.iter().any(|held| binding.is_chord_key(*held)) {
            self.blocked = false;
            return (self.end(Action::Finish), consume);
        }
        if down && !chord_key {
            self.blocked = true;
            return (self.end(Action::Cancel), consume);
        }
        if down && self.recording && key != 0 && key == binding.key {
            self.suppressed = key;
            return (Action::None, true);
        }
        if !down
            || self.blocked
            || self.recording
            || (binding.key != 0 && key != binding.key)
            || !self.required_modifiers_held()
        {
            return (Action::None, consume);
        }
        self.blocked = self.suppressed != 0
            || self.held.iter().any(|held| !binding.is_chord_key(*held))
            || is_down.is_some_and(|get| unexpected_modifier_held(binding, get))
            || !try_pressed();
        if self.blocked {
            return (Action::None, consume);
        }

        self.recording = true;
        self.suppressed = binding.key;
        (Action::Start, self.suppressed != 0)
    }

    pub(crate) fn set_binding(&mut self, binding: Binding) -> Action {
        if binding == self.binding {
            return Action::None;
        }
        self.binding = binding;
        self.clear_chord()
    }

    pub(crate) fn reset(&mut self) -> Action {
        self.suppressed = 0;
        self.clear_chord()
    }

    pub(crate) fn keys_released(&self, is_down: &dyn Fn(u32) -> bool) -> bool {
        let binding = self.binding;
        (binding.key == 0 || (binding.key != self.suppressed && !is_down(binding.key)))
            && (!binding.has(CONTROL) || !is_down(0x11))
            && (!binding.has(SHIFT) || !is_down(0x10))
            && (!binding.has(ALT) || !is_down(0x12))
    }

    fn update_held(&mut self, key: u32, down: bool) -> bool {
        let held = self.held.iter().position(|held| *held == key);
        match (down, held) {
            (true, None) => self.held.push(key),
            (false, Some(position)) => {
                self.held.swap_remove(position);
            }
            _ => return false,
        }
        true
    }

    fn clear_chord(&mut self) -> Action {
        self.held.clear();
        self.blocked = false;
        self.end(Action::Cancel)
    }

    fn end(&mut self, action: Action) -> Action {
        if std::mem::take(&mut self.recording) {
            action
        } else {
            Action::None
        }
    }

    fn required_modifiers_held(&self) -> bool {
        let held = self
            .held
            .iter()
            .fold(0, |mask, key| mask | modifier_for(*key));
        held & self.binding.modifiers == self.binding.modifiers
    }
}

#[cfg(any(windows, test))]
fn unexpected_modifier_held(binding: Binding, is_down: &dyn Fn(u32) -> bool) -> bool {
    (!binding.has(CONTROL) && is_down(0x11))
        || (!binding.has(SHIFT) && is_down(0x10))
        || (!binding.has(ALT) && is_down(0x12))
        || is_down(0x5b)
        || is_down(0x5c)
}

#[cfg(any(windows, test))]
fn modifier_for(key: u32) -> u32 {
    match key {
        0x10 | 0xa0 | 0xa1 => SHIFT,
        0x11 | 0xa2 | 0xa3 => CONTROL,
        0x12 | 0xa4 | 0xa5 => ALT,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: u32 = 0xa2;
    const SHIFT: u32 = 0xa1;
    const ALT: u32 = 0xa5;
    const K: u32 = 0x4b;
    const TAB: u32 = 0x09;
    const F8: u32 = 0x77;

    fn update(chord: &mut Shortcut, key: u32, down: bool) -> (Action, bool) {
        chord.observe(key, down, None, || true)
    }

    #[test]
    fn default_chord_works_in_either_order_and_waits_for_all_keys() {
        for (first, second) in [(CTRL, SHIFT), (SHIFT, CTRL)] {
            let mut chord = Shortcut::new(Binding::default());
            assert_eq!(update(&mut chord, first, true), (Action::None, false));
            assert_eq!(update(&mut chord, second, true), (Action::Start, false));
            assert_eq!(update(&mut chord, second, true), (Action::None, false));
            assert_eq!(update(&mut chord, first, false), (Action::None, false));
            assert_eq!(update(&mut chord, second, false), (Action::Finish, false));
        }
    }

    #[test]
    fn custom_key_is_suppressed_until_released_even_after_cancel() {
        let mut chord = Shortcut::new(Binding::parse("Ctrl+Shift+K").unwrap());
        assert_eq!(update(&mut chord, CTRL, true), (Action::None, false));
        assert_eq!(update(&mut chord, SHIFT, true), (Action::None, false));
        assert_eq!(update(&mut chord, K, true), (Action::Start, true));
        assert!(!chord.keys_released(&|_| false));
        assert_eq!(update(&mut chord, TAB, true), (Action::Cancel, false));
        assert_eq!(update(&mut chord, K, true), (Action::None, true));
        assert_eq!(update(&mut chord, K, false), (Action::None, true));
        assert!(chord.keys_released(&|_| false));
        assert_eq!(update(&mut chord, K, false), (Action::None, false));
    }

    #[test]
    fn declined_or_premature_main_is_never_swallowed() {
        let mut chord = Shortcut::new(Binding::parse("Ctrl+K").unwrap());
        assert_eq!(update(&mut chord, K, true), (Action::None, false));
        assert_eq!(update(&mut chord, CTRL, true), (Action::None, false));
        assert_eq!(update(&mut chord, K, true), (Action::None, false));
        assert_eq!(update(&mut chord, K, false), (Action::None, false));
        assert_eq!(
            chord.observe(K, true, None, || false),
            (Action::None, false)
        );
        assert_eq!(update(&mut chord, K, false), (Action::None, false));
        assert_eq!(update(&mut chord, K, true), (Action::None, false));
    }

    #[test]
    fn rebinding_cancels_without_leaking_old_main_release() {
        let mut chord = Shortcut::new(Binding::parse("Alt+K").unwrap());
        assert_eq!(update(&mut chord, ALT, true), (Action::None, false));
        assert_eq!(update(&mut chord, K, true), (Action::Start, true));
        assert_eq!(
            chord.set_binding(Binding::parse("F8").unwrap()),
            Action::Cancel
        );
        assert_eq!(update(&mut chord, K, false), (Action::None, true));
        assert_eq!(update(&mut chord, F8, true), (Action::Start, true));
        assert_eq!(update(&mut chord, F8, false), (Action::Finish, true));
    }

    #[test]
    fn loses_stale_chord_and_resets_on_session_change() {
        let mut chord = Shortcut::new(Binding::default());
        assert_eq!(update(&mut chord, CTRL, true), (Action::None, false));
        assert_eq!(update(&mut chord, SHIFT, true), (Action::Start, false));
        assert_eq!(
            chord.observe(CTRL, true, Some(&|_| false), || true),
            (Action::Cancel, false)
        );
        assert_eq!(
            chord.observe(SHIFT, true, Some(&|key| key == CTRL), || true),
            (Action::Start, false)
        );
        assert_eq!(chord.reset(), Action::Cancel);
        assert_eq!(chord.reset(), Action::None);
    }

    #[test]
    fn binding_preserves_winforms_json_and_rejects_invalid_shortcuts() {
        let binding: Binding = serde_json::from_str(r#"{"Modifiers":3,"Key":75}"#).unwrap();
        assert_eq!(binding.label(), "Ctrl+Shift+K");
        assert_eq!(
            serde_json::to_string(&binding).unwrap(),
            r#"{"Modifiers":3,"Key":75}"#
        );
        assert_eq!(
            Binding::parse("shift+alt+space").unwrap().label(),
            "Shift+Alt+Space"
        );
        for invalid in [
            "Ctrl",
            "K",
            "Ctrl+Ctrl+K",
            "Ctrl+K+L",
            "Win+F8",
            "F13",
            "Alt+",
        ] {
            assert!(Binding::parse(invalid).is_err(), "{invalid}");
        }
    }
}
