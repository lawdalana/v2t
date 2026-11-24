use std::{io, path::Path};

use serde::{Deserialize, Serialize};

use super::SpeechOptions;
use crate::{config, inference::MIXED};

const FILE: &str = "macos-settings.json";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, rename_all = "snake_case")]
pub(super) struct Preferences {
    pub language: Language,
    pub shortcut: Shortcut,
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
    ControlF9,
    RightCommandOption,
    Off,
}

impl Shortcut {
    pub(super) const OPTIONS: [Self; 5] = [
        Self::ControlShiftSpace,
        Self::CommandShiftSpace,
        Self::ControlF9,
        Self::RightCommandOption,
        Self::Off,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ControlShiftSpace => "⌃ Control + ⇧ Shift + Space",
            Self::CommandShiftSpace => "⌘ Command + ⇧ Shift + Space",
            Self::ControlF9 => "⌃ Control + F9",
            Self::RightCommandOption => "Right Command + Right Option (v2t active)",
            Self::Off => "Off",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_shift_space_is_a_saved_shortcut_option() {
        let preferences: Preferences =
            serde_json::from_str(r#"{"shortcut":"command_shift_space"}"#).unwrap();
        assert!(Shortcut::OPTIONS.contains(&preferences.shortcut));
        assert_eq!(
            serde_json::to_value(preferences).unwrap()["shortcut"],
            "command_shift_space"
        );
    }

    #[test]
    fn preferences_round_trip_and_use_selected_speech_language() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path();
        assert_eq!(Preferences::load(path).unwrap(), Preferences::default());

        let preferences = Preferences {
            language: Language::Thai,
            shortcut: Shortcut::ControlF9,
        };
        preferences.save(path).unwrap();
        let loaded = Preferences::load(path).unwrap();
        assert_eq!(loaded, preferences);
        assert_eq!(loaded.speech().language, Some("th"));
    }
}
