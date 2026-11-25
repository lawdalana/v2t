use std::{
    io::{self, IsTerminal, Read},
    path::Path,
    time::Duration,
};

use reqwest::Url;
use security_framework::passwords::{get_generic_password, set_generic_password};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{
    config,
    inference::{Inference, InferenceError, LiteLlmCorrectionOptions, valid_bearer_token},
};

const SETTINGS: &str = "litellm-correction.json";
const KEYCHAIN_SERVICE: &str = "com.lawdalana.v2t.litellm";
const TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Deserialize, Serialize)]
struct Settings {
    base_url: String,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

impl Settings {
    fn endpoint(&self) -> io::Result<Url> {
        let mut url = Url::parse(&self.base_url)
            .map_err(|_| config::invalid("Enter a valid HTTPS LiteLLM base URL."))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(config::invalid("Enter a valid HTTPS LiteLLM base URL."));
        }
        if !self
            .model
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            || self.model.len() > 160
            || !self.model.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
            })
        {
            return Err(config::invalid("Enter a LiteLLM model alias."));
        }
        if self.reasoning_effort.as_deref().is_some_and(|effort| {
            effort.is_empty()
                || effort.len() > 32
                || !effort
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        }) {
            return Err(config::invalid(
                "Enter a LiteLLM reasoning effort such as low.",
            ));
        }
        if url.path() == "/" {
            url.set_path("/v1");
        }
        url.path_segments_mut()
            .map_err(|_| config::invalid("Enter a valid HTTPS LiteLLM base URL."))?
            .pop_if_empty()
            .extend(["chat", "completions"]);
        Ok(url)
    }

    fn correction_options(&self) -> LiteLlmCorrectionOptions<'_> {
        LiteLlmCorrectionOptions {
            model: &self.model,
            reasoning_effort: self.reasoning_effort.as_deref(),
        }
    }
}

pub(super) struct Connection {
    client: Inference,
    endpoint: Url,
    settings: Settings,
    key: Zeroizing<Vec<u8>>,
}

impl Connection {
    pub(super) fn load(dir: &Path) -> super::Result<Self> {
        let bytes = config::read_bounded(&dir.join(SETTINGS), 4096)?
            .ok_or("Set up LiteLLM correction before selecting it.")?;
        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|_| config::invalid("Saved LiteLLM settings are invalid."))?;
        let endpoint = settings.endpoint()?;
        let key = Zeroizing::new(
            get_generic_password(KEYCHAIN_SERVICE, endpoint.as_str())
                .map_err(|_| "Could not read the LiteLLM key from macOS Keychain.")?,
        );
        Ok(Self {
            client: Inference::new()?,
            endpoint,
            settings,
            key,
        })
    }

    pub(super) fn model(&self) -> &str {
        &self.settings.model
    }

    pub(super) fn correct(
        &self,
        text: &str,
        protected: &[String],
    ) -> Result<Option<String>, InferenceError> {
        let key = std::str::from_utf8(&self.key)
            .map_err(|_| InferenceError::InvalidInput("The saved LiteLLM key is invalid."))?;
        self.client.litellm_correction(
            self.endpoint.as_str(),
            self.settings.correction_options(),
            key,
            text,
            protected,
            TIMEOUT,
        )
    }
}

pub(super) fn setup(
    dir: &Path,
    base_url: String,
    model: String,
    reasoning_effort: Option<String>,
) -> super::Result<()> {
    let settings = Settings {
        base_url: base_url.trim().to_owned(),
        model: model.trim().to_owned(),
        reasoning_effort,
    };
    let endpoint = settings.endpoint()?;
    let stdin = io::stdin();
    if stdin.is_terminal() {
        return Err("Pipe the LiteLLM key into this command through standard input.".into());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    stdin.lock().take(4097).read_to_end(&mut bytes)?;
    let key = std::str::from_utf8(&bytes)
        .map(str::trim)
        .unwrap_or_default();
    if bytes.len() > 4096 || !valid_bearer_token(key) {
        return Err("Enter a valid LiteLLM key on standard input.".into());
    }

    let result = Inference::new()?.litellm_correction(
        endpoint.as_str(),
        settings.correction_options(),
        key,
        "I recieved a mesage from OpenRouter.",
        &["OpenRouter".into()],
        TIMEOUT,
    )?;
    if result.is_none() {
        return Err("LiteLLM did not return a usable correction for the test sentence.".into());
    }
    set_generic_password(KEYCHAIN_SERVICE, endpoint.as_str(), key.as_bytes())
        .map_err(|_| "Could not save the LiteLLM key in macOS Keychain.")?;
    config::save_json(&dir.join(SETTINGS), &settings)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_configured_reasoning_effort() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "base_url": "https://example.com/v1",
            "model": "test-model",
            "reasoning_effort": "low",
        }))
        .unwrap();
        assert_eq!(settings.correction_options().reasoning_effort, Some("low"));
        assert_eq!(
            serde_json::to_value(settings).unwrap()["reasoning_effort"],
            "low"
        );
    }

    #[test]
    fn lite_llm_uses_the_configured_https_gateway_path() {
        let gateway = |base: &str| Settings {
            base_url: base.to_owned(),
            model: "test-model".to_owned(),
            reasoning_effort: None,
        };
        assert_eq!(
            gateway("https://example.com/proxy/v1/")
                .endpoint()
                .unwrap()
                .as_str(),
            "https://example.com/proxy/v1/chat/completions"
        );
        assert_eq!(
            gateway("https://example.com").endpoint().unwrap().as_str(),
            "https://example.com/v1/chat/completions"
        );
        for url in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com?v=1",
        ] {
            assert!(gateway(url).endpoint().is_err());
        }
    }
}
