use crate::{inference::SpeechProvider, shortcut::Binding};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    env, fs, io,
    io::Read,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const CONFIG_LIMIT: u64 = 640 * 1024;
const DOTENV_LIMIT: u64 = 64 * 1024;
const OPENROUTER_NAMES: [&str; 3] = [
    "OPENROUTER_API_KEY",
    "OPENROUTER_KEY_TOKEN",
    "OPENROUTER_KEY",
];

#[derive(Clone)]
pub struct Settings {
    pub provider: SpeechProvider,
    pub language: Option<String>,
    pub open_ai_key: Zeroizing<String>,
    pub open_router_key: Zeroizing<String>,
    pub enable_mai_backup: bool,
    pub shortcut: Binding,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: SpeechProvider::Local,
            language: Some("th+en".into()),
            open_ai_key: Zeroizing::default(),
            open_router_key: Zeroizing::default(),
            enable_mai_backup: true,
            shortcut: Binding::default(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredSettings {
    #[serde(rename = "Provider")]
    provider: u8,
    #[serde(rename = "Language")]
    language: Option<String>,
    #[serde(rename = "OpenAiKey")]
    open_ai_key: Option<String>,
    #[serde(rename = "OpenRouterKey")]
    open_router_key: Option<String>,
    #[serde(rename = "EnableMaiBackup")]
    enable_mai_backup: Option<bool>,
    #[serde(rename = "Shortcut")]
    shortcut: Option<Binding>,
}

impl Settings {
    pub fn load(dir: &Path) -> io::Result<Self> {
        let Some(bytes) = read_bounded(&dir.join("settings.json"), CONFIG_LIMIT)? else {
            return Ok(Self::default());
        };
        Self::decode(&bytes, unprotect)
    }

    fn decode(
        bytes: &[u8],
        decrypt: impl Fn(Option<String>) -> io::Result<Zeroizing<String>>,
    ) -> io::Result<Self> {
        let stored: StoredSettings =
            serde_json::from_slice(bytes).map_err(|_| invalid("Saved settings are invalid."))?;
        let provider = match stored.provider {
            0 => SpeechProvider::Local,
            1 => SpeechProvider::OpenAi,
            2 => SpeechProvider::OpenRouter,
            _ => return Err(invalid("Saved speech provider is invalid.")),
        };
        if !matches!(
            stored.language.as_deref(),
            None | Some("th+en" | "th" | "en")
        ) {
            return Err(invalid("Saved speech language is invalid."));
        }
        Ok(Self {
            provider,
            language: stored.language,
            open_ai_key: decrypt(stored.open_ai_key)?,
            open_router_key: decrypt(stored.open_router_key)?,
            enable_mai_backup: stored.enable_mai_backup.unwrap_or(false),
            shortcut: stored
                .shortcut
                .filter(|binding| binding.is_valid())
                .unwrap_or_default(),
        })
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        if !self.open_ai_key.is_empty() && !bearer_key(&self.open_ai_key) {
            return Err(invalid("The OpenAI key contains invalid characters."));
        }
        if !self.open_router_key.is_empty() && !router_key(&self.open_router_key) {
            return Err(invalid(
                "Enter a valid OpenRouter key or leave it blank to use your .env.",
            ));
        }
        let stored = StoredSettings {
            provider: match self.provider {
                SpeechProvider::Local => 0,
                SpeechProvider::OpenAi => 1,
                SpeechProvider::OpenRouter => 2,
            },
            language: self.language.clone(),
            open_ai_key: protect(&self.open_ai_key)?,
            open_router_key: protect(&self.open_router_key)?,
            enable_mai_backup: Some(self.enable_mai_backup),
            shortcut: Some(self.shortcut),
        };
        save_json(&dir.join("settings.json"), &stored)
    }

    pub fn router_key(&self, app_dir: &Path) -> Option<Zeroizing<String>> {
        if !self.open_router_key.trim().is_empty() {
            return router_key(self.open_router_key.trim())
                .then(|| Zeroizing::new(self.open_router_key.trim().into()));
        }
        for name in OPENROUTER_NAMES {
            if let Ok(value) = env::var(name) {
                let value = Zeroizing::new(value);
                if router_key(value.trim()) {
                    return Some(Zeroizing::new(value.trim().into()));
                }
            }
        }
        read_dotenv(&app_dir.join(".env")).or_else(|| {
            (app_dir.file_name()? == "publish").then_some(())?;
            read_dotenv(&app_dir.parent()?.join(".env"))
        })
    }
}

pub fn data_dir() -> io::Result<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join("v2t"))
        .ok_or_else(|| io::Error::other("Windows local application storage is unavailable."))
}

pub fn read_bounded(file: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let mut reader = match fs::File::open(file) {
        Ok(file) => file.take(limit + 1),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("A saved file exceeds its size limit."));
    }
    Ok(Some(bytes))
}

pub fn save_json<T: Serialize>(file: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("Could not encode the settings."))?;
    if bytes.len() as u64 > CONFIG_LIMIT {
        return Err(invalid("The settings would exceed the storage limit."));
    }
    atomic_write(file, &bytes)
}

pub fn atomic_write(file: &Path, bytes: &[u8]) -> io::Result<()> {
    use io::Write as _;
    let dir = file
        .parent()
        .ok_or_else(|| invalid("An invalid file path was provided."))?;
    fs::create_dir_all(dir)?;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce)
        .map_err(|_| io::Error::other("Could not create a temporary file name."))?;
    let temp = dir.join(format!(".v2t-{:016x}.tmp", u64::from_ne_bytes(nonce)));
    let result = (|| {
        let mut output = fs::File::options()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temp, file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn protect(value: &str) -> io::Result<Option<String>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    #[cfg(windows)]
    {
        Ok(Some(
            STANDARD.encode(crate::system::protect(value.as_bytes())?),
        ))
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::other("Saving credentials requires Windows."))
    }
}

fn unprotect(value: Option<String>) -> io::Result<Zeroizing<String>> {
    let Some(value) = value else {
        return Ok(Zeroizing::default());
    };
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| invalid("A saved API key is invalid."))?;
    #[cfg(windows)]
    {
        crate::system::unprotect(&bytes)
    }
    #[cfg(not(windows))]
    {
        let _ = bytes;
        Err(io::Error::other(
            "Reading saved credentials requires Windows.",
        ))
    }
}

pub fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_dotenv(path: &Path) -> Option<Zeroizing<String>> {
    let bytes = Zeroizing::new(read_bounded(path, DOTENV_LIMIT).ok()??);
    let text = str::from_utf8(&bytes).ok()?.trim_start_matches('\u{feff}');
    dotenv_router_key(text).map(|key| Zeroizing::new(key.into()))
}

fn dotenv_router_key(text: &str) -> Option<&str> {
    let mut found = [None; OPENROUTER_NAMES.len()];
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, body)) = line.split_once('=') else {
            continue;
        };
        let Some(index) = OPENROUTER_NAMES
            .iter()
            .position(|candidate| *candidate == name.trim())
        else {
            continue;
        };
        let body = body.trim();
        let value = if let Some(quote) = body
            .chars()
            .next()
            .filter(|character| matches!(character, '\'' | '"'))
        {
            let Some((value, remainder)) = body[1..].split_once(quote) else {
                continue;
            };
            let valid_suffix = remainder.is_empty()
                || remainder.starts_with(char::is_whitespace)
                    && remainder.trim_start().starts_with('#');
            if !valid_suffix {
                continue;
            }
            value
        } else {
            let end = body.find(char::is_whitespace).unwrap_or(body.len());
            let remainder = body[end..].trim();
            if !remainder.is_empty() && !remainder.starts_with('#') {
                continue;
            }
            &body[..end]
        };
        if router_key(value) {
            found[index] = Some(value);
        }
    }
    found.into_iter().flatten().next()
}

fn router_key(token: &str) -> bool {
    token.len() <= 512
        && token.strip_prefix("sk-or-v1-").is_some_and(|body| {
            body.len() >= 16
                && body
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        })
}

fn bearer_key(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 512
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.~+/=".contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_the_legacy_numeric_provider_and_missing_backup() {
        let bytes = br#"{"Provider":2,"Language":"th+en","OpenAiKey":null,"OpenRouterKey":null,"Shortcut":{"Modifiers":3,"Key":0}}"#;
        let settings = Settings::decode(bytes, |_| Ok(Zeroizing::default())).unwrap();
        assert!(matches!(settings.provider, SpeechProvider::OpenRouter));
        assert_eq!(settings.language.as_deref(), Some("th+en"));
        assert!(!settings.enable_mai_backup);
        assert_eq!(settings.shortcut.label(), "Ctrl+Shift");
    }

    #[test]
    fn dotenv_matches_precedence_without_parsing_shell() {
        let data = "OPENROUTER_KEY_TOKEN='sk-or-v1-fixture_token_two' # comment\nexport OPENROUTER_API_KEY=\"sk-or-v1-fixture_token_one\"\n";
        assert_eq!(dotenv_router_key(data), Some("sk-or-v1-fixture_token_one"));
        assert_eq!(dotenv_router_key("OPENROUTER_API_KEY=$(echo bad)"), None);
        assert_eq!(
            dotenv_router_key("OPENROUTER_API_KEY=sk-or-v1-fixture_token_one#comment"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn persists_without_plaintext_and_can_replace_existing_settings() {
        let dir = env::temp_dir().join(format!("v2t-dpapi-test-{}", std::process::id()));
        let mut settings = Settings {
            open_ai_key: Zeroizing::new("synthetic-api-fixture".into()),
            ..Settings::default()
        };
        settings.save(&dir).unwrap();
        assert!(
            !fs::read_to_string(dir.join("settings.json"))
                .unwrap()
                .contains("synthetic-api-fixture")
        );
        settings.language = Some("th".into());
        settings.save(&dir).unwrap();
        let reloaded = Settings::load(&dir).unwrap();
        assert_eq!(reloaded.language.as_deref(), Some("th"));
        assert_eq!(reloaded.open_ai_key.as_str(), "synthetic-api-fixture");
        fs::remove_dir_all(dir).unwrap();
    }
}
