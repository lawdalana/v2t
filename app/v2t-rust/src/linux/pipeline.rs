use std::{
    env, fs, io,
    path::Path,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

use super::{Result, SpeechOptions, Storage, audio};
use crate::{
    config::Settings,
    inference::{
        Inference, InferenceError, MAI_MODEL, SpeechProvider, SpeechResult, valid_bearer_token,
    },
    models::{CORRECTION_MODEL_FILE, LocalModels},
    speech_gate,
    text::{self, Corrections},
    usage::{Entry, Log, local_now},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const CORRECTION_STARTUP_TIMEOUT: Duration = Duration::from_secs(55);
const CORRECTION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Credentials {
    direct: Option<Zeroizing<String>>,
    backup: Option<Zeroizing<String>>,
}

fn resolve_credentials(options: &SpeechOptions) -> Result<Credentials> {
    let router_key = || -> Result<Zeroizing<String>> {
        let executable = env::current_exe()?;
        let app_dir = executable.parent().unwrap_or(Path::new("/"));
        Settings::default()
            .router_key(app_dir)
            .ok_or_else(|| "Set OPENROUTER_API_KEY to a valid key before using OpenRouter.".into())
    };
    match options.provider {
        SpeechProvider::Local if options.mai_backup => Ok(Credentials {
            direct: None,
            backup: Some(router_key()?),
        }),
        SpeechProvider::Local => Ok(Credentials::default()),
        SpeechProvider::OpenRouter => Ok(Credentials {
            direct: Some(router_key()?),
            backup: None,
        }),
        SpeechProvider::OpenAi => {
            let key = env::var("OPENAI_API_KEY")
                .map(Zeroizing::new)
                .unwrap_or_default();
            if !valid_bearer_token(key.trim()) {
                return Err("Set OPENAI_API_KEY to a valid key before using OpenAI.".into());
            }
            Ok(Credentials {
                direct: Some(Zeroizing::new(key.trim().to_owned())),
                backup: None,
            })
        }
    }
}

pub(super) fn validate_credentials(options: &SpeechOptions) -> Result<()> {
    resolve_credentials(options).map(|_| ())
}

pub(super) fn transcribe(
    storage: &Storage,
    options: &SpeechOptions,
    wave: &[u8],
) -> Result<Option<String>> {
    if !speech_gate::has_speech(wave) {
        eprintln!("No speech detected.");
        return Ok(None);
    }
    let credentials = resolve_credentials(options)?;
    let corrections = Corrections::load(&storage.config)?;
    let mut models = LocalModels::new(storage.data.clone())?;
    let mut log = Log::new(storage.data.clone());
    if with_log_lock(&storage.data, || log.cleanup()).is_err() {
        eprintln!("Could not clean up the local usage log.");
    }

    let speech = recognize(
        wave,
        &mut models,
        options,
        &credentials,
        &storage.data,
        &log,
    )?;
    let (mapped, protected) = corrections.apply(&text::for_insertion(&speech.text));
    if mapped.is_empty() {
        eprintln!("No speech was recognized.");
        return Ok(None);
    }
    if !options.correct || !corrections.enable_ai {
        return Ok(Some(mapped));
    }
    if !models.correction_available() {
        eprintln!("Local AI correction is unavailable. Using dictionary replacements.");
        return Ok(Some(mapped));
    }
    eprintln!("Loading the local AI correction model…");
    let started = local_now();
    let elapsed = Instant::now();
    let result = models
        .warm_correction(CORRECTION_STARTUP_TIMEOUT)
        .and_then(|()| {
            eprintln!("Correcting text with local AI…");
            models.correct(&mapped, &protected, CORRECTION_TIMEOUT)
        });
    let outcome = match &result {
        Ok(Some(_)) => "ok",
        Ok(None) => "fallback",
        Err(InferenceError::Timeout(_)) => "canceled",
        Err(_) => "failed",
    };
    save_entry(
        &storage.data,
        &log,
        Entry {
            started_at: started,
            stage: "correction",
            provider: "Local",
            model: CORRECTION_MODEL_FILE,
            outcome,
            audio_seconds: None,
            elapsed: elapsed.elapsed().as_secs_f64(),
            billed_seconds: None,
            cost: Some(0.0),
            cost_kind: "local",
        },
    );
    match result {
        Ok(Some(corrected)) => Ok(Some(text::for_insertion(&corrected))),
        _ => {
            eprintln!("Local AI correction did not complete. Using dictionary replacements.");
            Ok(Some(mapped))
        }
    }
}

fn recognize(
    wave: &[u8],
    models: &mut LocalModels,
    options: &SpeechOptions,
    credentials: &Credentials,
    data: &Path,
    log: &Log,
) -> Result<SpeechResult> {
    let seconds = (wave.len() - audio::HEADER) as f64 / audio::BYTES_PER_SECOND as f64;
    let started = local_now();
    let elapsed = Instant::now();
    let mut attempt = SpeechAttempt {
        started,
        elapsed,
        seconds,
    };
    eprintln!("Transcribing…");
    if let Some(key) = &credentials.direct {
        let result = Inference::new()?.speech(
            options.provider,
            wave,
            key,
            options.language,
            REQUEST_TIMEOUT,
        );
        attempt.log(
            data,
            log,
            options.provider,
            options.provider.model().unwrap_or(""),
            &result,
        );
        return Ok(result?);
    }

    let local_budget = match credentials.backup {
        Some(_) => Duration::from_secs_f64((5.0 + seconds / 4.0).min(60.0)),
        None => REQUEST_TIMEOUT,
    };
    let result = models.speech(wave, options.language, local_budget);
    let model = models
        .loaded_speech()
        .map_or("Local Whisper", |model| model.file);
    attempt.log(data, log, SpeechProvider::Local, model, &result);
    let Some(key) = &credentials.backup else {
        return Ok(result?);
    };
    if !result.as_ref().is_err_and(InferenceError::can_fallback) {
        return Ok(result?);
    }

    eprintln!("Local transcription failed. Trying MAI backup with ZDR…");
    attempt.started = local_now();
    attempt.elapsed = Instant::now();
    let result = Inference::new()?.mai(wave, key, options.language, REQUEST_TIMEOUT);
    attempt.log(data, log, SpeechProvider::OpenRouter, MAI_MODEL, &result);
    Ok(result?)
}

struct SpeechAttempt {
    started: time::OffsetDateTime,
    elapsed: Instant,
    seconds: f64,
}

impl SpeechAttempt {
    fn log(
        &self,
        data: &Path,
        log: &Log,
        provider: SpeechProvider,
        model: &str,
        result: &std::result::Result<SpeechResult, InferenceError>,
    ) {
        let response = result.as_ref().ok();
        let (cost, cost_kind) = if provider == SpeechProvider::Local {
            (Some(0.0), "local")
        } else if let Some(cost) = response.and_then(|response| response.cost_usd) {
            (Some(cost), "reported")
        } else if response.is_some() && provider == SpeechProvider::OpenAi {
            (Some(self.seconds / 60.0 * 0.0045), "estimated")
        } else {
            (None, "unknown")
        };
        let outcome = match result {
            Ok(result) if result.text.is_empty() => "empty",
            Ok(_) => "ok",
            Err(InferenceError::Timeout(_)) => "canceled",
            Err(_) => "failed",
        };
        save_entry(
            data,
            log,
            Entry {
                started_at: self.started,
                stage: "speech",
                provider: provider.label(),
                model,
                outcome,
                audio_seconds: Some(self.seconds),
                elapsed: self.elapsed.elapsed().as_secs_f64(),
                billed_seconds: response.and_then(|result| result.billed_seconds),
                cost,
                cost_kind,
            },
        );
    }
}

fn save_entry(data: &Path, log: &Log, row: Entry<'_>) {
    if with_log_lock(data, || log.append(row)).is_err() {
        eprintln!("Could not write the local usage log.");
    }
}

fn with_log_lock(data: &Path, operation: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    fs::create_dir_all(data)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(data.join(".usage.lock"))?;
    lock.lock()?;
    operation()
}
