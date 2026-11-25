use std::{io, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    config,
    inference::MIXED,
    linux::{CorrectionMode, SpeechOptions},
};

const FILE: &str = "macos-settings.json";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "snake_case")]
pub(super) struct Preferences {
    pub language: Language,
    #[serde(alias = "shortcut")]
    pub shortcuts: Shortcuts,
    pub ai_correction: bool,
    pub correction_provider: CorrectionProvider,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(from = "StoredShortcuts", into = "Vec<Shortcut>")]
pub(super) struct Shortcuts(u16);

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredShortcuts {
    Multiple(Vec<Shortcut>),
    Legacy(Shortcut),
}

impl Shortcuts {
    pub(super) fn contains(self, shortcut: Shortcut) -> bool {
        self.0 & shortcut.bit() != 0
    }

    pub(super) fn iter(self) -> impl Iterator<Item = Shortcut> {
        Shortcut::OPTIONS
            .into_iter()
            .filter(move |&shortcut| self.contains(shortcut))
    }
}

impl Default for Shortcuts {
    fn default() -> Self {
        Self(Shortcut::default().bit())
    }
}

impl From<StoredShortcuts> for Shortcuts {
    fn from(stored: StoredShortcuts) -> Self {
        match stored {
            StoredShortcuts::Multiple(shortcuts) => shortcuts.into_iter().collect(),
            StoredShortcuts::Legacy(shortcut) => Self(shortcut.bit()),
        }
    }
}

impl FromIterator<Shortcut> for Shortcuts {
    fn from_iter<I: IntoIterator<Item = Shortcut>>(shortcuts: I) -> Self {
        let mut bits = 0;
        for shortcut in shortcuts {
            bits |= shortcut.bit();
        }
        Self(bits)
    }
}

impl From<Shortcuts> for Vec<Shortcut> {
    fn from(shortcuts: Shortcuts) -> Self {
        shortcuts.iter().collect()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum CorrectionProvider {
    #[default]
    Local,
    #[serde(rename = "litellm")]
    LiteLlm,
}

impl Preferences {
    pub(super) fn load(dir: &Path) -> io::Result<Self> {
        let Some(bytes) = config::read_bounded(&dir.join(FILE), 4096)? else {
            return Ok(Self::default());
        };
        serde_json::from_slice(&bytes)
            .map_err(|_| config::invalid("Saved v2t settings are invalid."))
    }

    pub(super) fn save(self, dir: &Path) -> io::Result<()> {
        config::save_json(&dir.join(FILE), &self)
    }

    pub(super) fn speech(self) -> SpeechOptions {
        SpeechOptions {
            language: self.language.code(),
            correct: match (self.ai_correction, self.correction_provider) {
                (false, _) => CorrectionMode::Disabled,
                (true, CorrectionProvider::Local) => CorrectionMode::Enabled,
                (true, CorrectionProvider::LiteLlm) => CorrectionMode::LiteLlm,
            },
            ..SpeechOptions::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Language {
    #[default]
    Mixed,
    Automatic,
    Thai,
    English,
}

impl Language {
    pub(super) const OPTIONS: [Self; 4] = [Self::Mixed, Self::Automatic, Self::Thai, Self::English];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Mixed => "Thai + English (mixed)",
            Self::Automatic => "Detect automatically",
            Self::Thai => "Thai",
            Self::English => "English",
        }
    }

    fn code(self) -> Option<&'static str> {
        match self {
            Self::Mixed => Some(MIXED),
            Self::Automatic => None,
            Self::Thai => Some("th"),
            Self::English => Some("en"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Shortcut {
    #[default]
    ControlShiftSpace,
    CommandShiftSpace,
    CommandOptionSpace,
    ControlF9,
    RightOptionSpace,
    RightCommandOption,
    Off,
}

impl Shortcut {
    pub(super) const OPTIONS: [Self; 6] = [
        Self::ControlShiftSpace,
        Self::CommandShiftSpace,
        Self::CommandOptionSpace,
        Self::ControlF9,
        Self::RightOptionSpace,
        Self::RightCommandOption,
    ];

    fn bit(self) -> u16 {
        if self == Self::Off {
            0
        } else {
            1 << self as u8
        }
    }

    pub(super) fn uses_carbon(self) -> bool {
        matches!(
            self,
            Self::ControlShiftSpace
                | Self::CommandShiftSpace
                | Self::CommandOptionSpace
                | Self::ControlF9
        )
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ControlShiftSpace => "⌃ Control + ⇧ Shift + Space",
            Self::CommandShiftSpace => "⌘ Command + ⇧ Shift + Space",
            Self::CommandOptionSpace => "⌘ Command + ⌥ Option + Space",
            Self::ControlF9 => "⌃ Control + F9",
            Self::RightOptionSpace => "Right ⌥ Option + Space",
            Self::RightCommandOption => "Right Command + Right Option (v2t focus)",
            Self::Off => "Off",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_multiple_hold_to_dictate_shortcuts() {
        let settings: Preferences =
            serde_json::from_str(r#"{"shortcuts":["command_shift_space","right_option_space"]}"#)
                .unwrap();
        assert_eq!(
            serde_json::to_value(settings).unwrap()["shortcuts"],
            serde_json::json!(["command_shift_space", "right_option_space"])
        );
    }

    #[test]
    fn command_shift_space_is_a_saved_shortcut_option() {
        let preferences: Preferences =
            serde_json::from_str(r#"{"shortcut":"command_shift_space"}"#).unwrap();
        assert!(preferences.shortcuts.contains(Shortcut::CommandShiftSpace));
        assert_eq!(
            serde_json::to_value(preferences).unwrap()["shortcuts"],
            serde_json::json!(["command_shift_space"])
        );
    }

    #[test]
    fn command_option_space_is_a_saved_global_shortcut() {
        let preferences: Preferences =
            serde_json::from_str(r#"{"shortcuts":["command_option_space"]}"#).unwrap();
        assert!(preferences.shortcuts.iter().all(Shortcut::uses_carbon));
        assert_eq!(
            serde_json::to_value(preferences).unwrap()["shortcuts"],
            serde_json::json!(["command_option_space"])
        );
    }

    #[test]
    fn legacy_off_and_an_empty_selection_disable_all_shortcuts() {
        for json in [r#"{"shortcut":"off"}"#, r#"{"shortcuts":[]}"#] {
            let preferences: Preferences = serde_json::from_str(json).unwrap();
            assert_eq!(preferences.shortcuts.iter().next(), None);
        }
        assert_eq!(
            Preferences::default().shortcuts.iter().next(),
            Some(Shortcut::ControlShiftSpace)
        );
    }

    #[test]
    fn preferences_round_trip_and_use_selected_speech_language() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        assert_eq!(Preferences::load(path).unwrap(), Preferences::default());

        let preferences = Preferences {
            language: Language::Thai,
            shortcuts: [Shortcut::ControlF9].into_iter().collect(),
            ..Preferences::default()
        };
        preferences.save(path).unwrap();
        let loaded = Preferences::load(path).unwrap();
        assert_eq!(loaded, preferences);
        assert_eq!(loaded.speech().language, Some("th"));
    }

    #[test]
    fn saved_ai_correction_is_used_for_desktop_dictation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        let legacy: Preferences = serde_json::from_str(r#"{"language":"thai"}"#).unwrap();
        let enabled: Preferences =
            serde_json::from_str(r#"{"language":"thai","ai_correction":true}"#).unwrap();

        enabled.save(path).unwrap();
        let loaded = Preferences::load(path).unwrap();
        assert_ne!(loaded.speech().correct, legacy.speech().correct);
        assert!(!legacy.speech().correct.enabled(true));
        assert!(loaded.speech().correct.enabled(false));
        assert_eq!(serde_json::to_value(loaded).unwrap()["ai_correction"], true);
    }

    #[test]
    fn macos_settings_keep_the_selected_litellm_provider() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        let settings: Preferences =
            serde_json::from_str(r#"{"ai_correction":true,"correction_provider":"litellm"}"#)
                .unwrap();
        settings.save(path).unwrap();
        let loaded = Preferences::load(path).unwrap();
        assert!(loaded.speech().correct.enabled(false));
        assert_eq!(loaded.speech().correct, CorrectionMode::LiteLlm);
        assert_eq!(
            serde_json::to_value(loaded).unwrap()["correction_provider"],
            "litellm"
        );
    }
}
