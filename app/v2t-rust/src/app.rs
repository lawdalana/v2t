use crate::{
    config::Settings,
    inference::{Inference, InferenceError, MAI_MODEL, SpeechProvider, SpeechResult},
    models::{CORRECTION_MODEL_FILE, LocalModels},
    resource::Reason,
    speech_gate::{BYTES_PER_SECOND, HEADER},
    text,
    text::Corrections,
    usage::{Entry, Log, local_now},
};
use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Preferences {
    pub settings: Settings,
    pub corrections: Corrections,
    pub router_key: Option<Zeroizing<String>>,
}

impl Preferences {
    pub fn load(dir: &Path, executable: &Path) -> (Self, Option<String>) {
        let (settings, mut message) = match Settings::load(dir) {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings {
                    enable_mai_backup: false,
                    ..Settings::default()
                },
                Some(error.to_string()),
            ),
        };
        let corrections = Corrections::load(dir).unwrap_or_else(|error| {
            message.get_or_insert_with(|| error.to_string());
            Corrections {
                enable_ai: false,
                ..Corrections::default()
            }
        });
        let router_key = settings.router_key(executable);
        (
            Self {
                settings,
                corrections,
                router_key,
            },
            message,
        )
    }

    pub fn can_backup(&self) -> bool {
        self.settings.provider == SpeechProvider::Local
            && self.settings.enable_mai_backup
            && self.router_key.is_some()
    }

    pub fn key(&self) -> Option<&str> {
        match self.settings.provider {
            SpeechProvider::Local => None,
            SpeechProvider::OpenAi => {
                (!self.settings.open_ai_key.is_empty()).then(|| self.settings.open_ai_key.as_str())
            }
            SpeechProvider::OpenRouter => self.router_key.as_ref().map(|key| key.as_str()),
        }
    }
}

pub enum Command {
    Warm(Box<Preferences>, bool),
    Suspend,
    Dictate(Box<Dictation>),
    Maintenance,
}

pub struct Dictation {
    pub id: u64,
    pub canceled: Arc<AtomicBool>,
    pub preferences: Preferences,
    pub audio: Vec<u8>,
    pub reason: Reason,
}

pub enum Notice {
    Status(String),
    Unavailable(String),
    Models {
        speech: bool,
        preferred: bool,
        correction: bool,
    },
    Warmed,
    Installed(Result<(), String>),
    SegmentFinished {
        id: u64,
        result: Result<Transcript, String>,
    },
}

#[derive(Default)]
pub struct Transcript {
    pub text: String,
    pub backup: bool,
    pub correction_fallback: bool,
}

impl Transcript {
    pub fn append(&mut self, segment: Self) {
        text::append_transcript(&mut self.text, &segment.text);
        self.backup |= segment.backup;
        self.correction_fallback |= segment.correction_fallback;
    }
}

pub fn spawn(
    dir: PathBuf,
    notify: impl Fn(Notice) + Send + Sync + 'static,
) -> io::Result<Sender<Command>> {
    let (send, receive) = mpsc::channel();
    let notify = Arc::new(notify);
    thread::Builder::new()
        .name("v2t-models".into())
        .spawn(move || {
            // A stopped worker must release any recording waiting for queued segments.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let mut worker = Worker::new(dir, &*notify)?;
                worker.cleanup();
                while let Ok(command) = receive.recv() {
                    worker.handle(command);
                }
                Ok::<_, InferenceError>(())
            }));
            let error = match outcome {
                Ok(Ok(())) => return,
                Ok(Err(error)) => error.to_string(),
                Err(_) => "The speech worker stopped unexpectedly. Restart v2t.".into(),
            };
            drop(receive);
            notify(Notice::Unavailable(error));
        })?;
    Ok(send)
}

pub fn install(
    dir: PathBuf,
    language: Option<String>,
    correction: bool,
    notify: impl Fn(Notice) + Send + 'static,
) -> io::Result<()> {
    thread::Builder::new()
        .name("v2t-model-setup".into())
        .spawn(move || {
            let result = LocalModels::new(dir).and_then(|models| {
                let progress = |message: &str| notify(Notice::Status(message.into()));
                if correction {
                    models.install_correction(progress)
                } else {
                    models.install_speech(language.as_deref(), progress)
                }
            });
            notify(Notice::Installed(result.map_err(|error| error.to_string())));
        })?;
    Ok(())
}

struct Worker<'a> {
    models: LocalModels,
    cloud: Inference,
    usage: Log,
    warned_on_log: bool,
    notify: &'a dyn Fn(Notice),
}

impl<'a> Worker<'a> {
    fn new(dir: PathBuf, notify: &'a dyn Fn(Notice)) -> Result<Self, InferenceError> {
        Ok(Self {
            models: LocalModels::new(dir.clone())?,
            cloud: Inference::new()?,
            usage: Log::new(dir),
            warned_on_log: false,
            notify,
        })
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Warm(preferences, allow) => self.warm(&preferences, allow),
            Command::Suspend => self.models.suspend(),
            Command::Maintenance => self.cleanup(),
            Command::Dictate(dictation) => {
                if !dictation.canceled.load(Ordering::Acquire) {
                    let result = self.transcribe(&dictation);
                    if !dictation.canceled.load(Ordering::Acquire) {
                        (self.notify)(Notice::SegmentFinished {
                            id: dictation.id,
                            result,
                        });
                    }
                }
            }
        }
    }

    fn warm(&mut self, preferences: &Preferences, allow: bool) {
        let language = preferences.settings.language.as_deref();
        let speech = self.models.speech_available(language);
        let preferred = self.models.preferred_speech_installed(language);
        let correction = self.models.correction_available();
        (self.notify)(Notice::Models {
            speech,
            preferred,
            correction,
        });
        if preferences.settings.provider != SpeechProvider::Local {
            self.models.suspend_speech();
        }
        if !preferences.corrections.enable_ai {
            self.models.suspend_correction();
        }
        if !allow {
            (self.notify)(Notice::Warmed);
            return;
        }
        let shortcut = preferences.settings.shortcut.label();
        let mut summary = format!("Ready. Hold {shortcut} to speak.");
        let use_correction = correction && preferences.corrections.enable_ai;
        let mut correction_result = None;
        if preferences.settings.provider == SpeechProvider::Local {
            if speech {
                self.status(if use_correction {
                    "Loading local models…"
                } else {
                    "Loading local speech model…"
                });
                let speech_result = if use_correction {
                    let (speech_result, ai) =
                        self.models.warm_both(language, Duration::from_secs(55));
                    correction_result = Some(ai);
                    speech_result
                } else {
                    self.models.warm_speech(language, Duration::from_secs(55))
                };
                summary = match speech_result {
                    Ok(model) => {
                        let device = if self.models.speech_is_cuda() {
                            " on NVIDIA GPU"
                        } else {
                            " on CPU"
                        };
                        format!("{}{device}. Hold {shortcut} to speak.", model.name)
                    }
                    Err(error) => error.to_string(),
                };
            } else if preferences.can_backup() {
                summary = "Local model unavailable. MAI backup is ready.".into();
            } else {
                summary = "Install the local model or select a cloud provider.".into();
            }
        }
        if use_correction {
            let result = correction_result.unwrap_or_else(|| {
                self.status("Loading local AI correction model…");
                self.models.warm_correction(Duration::from_secs(55))
            });
            if result.is_err() {
                summary.push_str(" Dictionary correction is ready.");
            }
        }
        self.status(&summary);
        (self.notify)(Notice::Warmed);
    }

    fn transcribe(&mut self, dictation: &Dictation) -> Result<Transcript, String> {
        let audio = &dictation.audio;
        let preferences = &dictation.preferences;
        let provider = preferences.settings.provider;
        let audio_seconds = audio.len().saturating_sub(HEADER) as f64 / BYTES_PER_SECOND as f64;
        let language = preferences.settings.language.as_deref();
        let mut used_backup = false;

        let speech = if provider != SpeechProvider::Local {
            let key = preferences
                .key()
                .ok_or_else(|| format!("Add your {} API key in Settings.", provider.label()))?;
            let start = Measurement::new();
            let result = self
                .cloud
                .speech(provider, audio, key, language, Duration::from_secs(90));
            self.record_speech(
                start,
                provider,
                provider.model().unwrap_or(""),
                audio_seconds,
                &result,
            );
            result
        } else if preferences.can_backup() && dictation.reason != Reason::None {
            self.models.suspend();
            used_backup = true;
            self.backup(audio, preferences, audio_seconds, dictation.reason)
        } else {
            let budget = if preferences.can_backup() {
                Duration::from_secs_f64((5.0 + audio_seconds / 4.0).min(60.0))
            } else {
                Duration::from_secs(90)
            };
            let start = Measurement::new();
            let local = self.models.speech(audio, language, budget);
            let model = self
                .models
                .loaded_speech()
                .map_or("Local Whisper", |model| model.file);
            self.record_speech(start, provider, model, audio_seconds, &local);
            if local.as_ref().is_err_and(InferenceError::can_fallback)
                && preferences.can_backup()
                && !dictation.canceled.load(Ordering::Acquire)
            {
                used_backup = true;
                self.backup(audio, preferences, audio_seconds, Reason::None)
            } else {
                local
            }
        }
        .map_err(|error| error.to_string())?;

        let normalized = text::for_insertion(&speech.text);
        let (mut text, protected) = preferences.corrections.apply(&normalized);
        let mut correction_fallback = false;
        if !text.is_empty()
            && dictation.reason == Reason::None
            && preferences.corrections.enable_ai
            && !dictation.canceled.load(Ordering::Acquire)
            && self.models.correction_available()
        {
            let start = Measurement::new();
            let corrected = self
                .models
                .correct(&text, &protected, Duration::from_secs(5));
            let outcome = match &corrected {
                Ok(Some(_)) => "ok",
                Ok(None) => "fallback",
                Err(InferenceError::Timeout(_)) => "canceled",
                Err(_) => "failed",
            };
            self.record(Entry {
                started_at: start.date,
                stage: "correction",
                provider: "Local",
                model: CORRECTION_MODEL_FILE,
                outcome,
                audio_seconds: None,
                elapsed: start.time.elapsed().as_secs_f64(),
                billed_seconds: None,
                cost: Some(0.0),
                cost_kind: "local",
            });
            if let Ok(Some(corrected)) = corrected {
                text = text::for_insertion(&corrected);
            } else {
                correction_fallback = true;
            }
        }
        Ok(Transcript {
            text,
            backup: used_backup,
            correction_fallback,
        })
    }

    fn backup(
        &mut self,
        audio: &[u8],
        preferences: &Preferences,
        seconds: f64,
        reason: Reason,
    ) -> Result<SpeechResult, InferenceError> {
        if reason == Reason::None {
            self.status("Local could not transcribe. Trying MAI backup…");
        } else {
            self.status(&format!("Using MAI backup · {}…", reason.label()));
        }
        let key = preferences
            .router_key
            .as_ref()
            .expect("MAI eligibility checked");
        let start = Measurement::new();
        let result = self.cloud.mai(
            audio,
            key,
            preferences.settings.language.as_deref(),
            Duration::from_secs(90),
        );
        self.record_speech(
            start,
            SpeechProvider::OpenRouter,
            MAI_MODEL,
            seconds,
            &result,
        );
        result
    }

    fn record_speech(
        &mut self,
        start: Measurement,
        provider: SpeechProvider,
        model: &str,
        audio_seconds: f64,
        result: &Result<SpeechResult, InferenceError>,
    ) {
        let response = result.as_ref().ok();
        let (cost, cost_kind) = if provider == SpeechProvider::Local {
            (Some(0.0), "local")
        } else if let Some(cost) = response.and_then(|response| response.cost_usd) {
            (Some(cost), "reported")
        } else if response.is_some() && provider == SpeechProvider::OpenAi {
            (Some(audio_seconds / 60.0 * 0.0045), "estimated")
        } else {
            (None, "unknown")
        };
        let outcome = match result {
            Ok(result) if result.text.is_empty() => "empty",
            Ok(_) => "ok",
            Err(InferenceError::Timeout(_)) => "canceled",
            Err(_) => "failed",
        };
        self.record(Entry {
            started_at: start.date,
            stage: "speech",
            provider: provider.label(),
            model,
            outcome,
            audio_seconds: Some(audio_seconds),
            elapsed: start.time.elapsed().as_secs_f64(),
            billed_seconds: response.and_then(|response| response.billed_seconds),
            cost,
            cost_kind,
        });
    }

    fn status(&self, text: &str) {
        (self.notify)(Notice::Status(text.into()));
    }

    fn cleanup(&mut self) {
        if self.usage.cleanup().is_err() && !self.warned_on_log {
            self.warned_on_log = true;
            self.status("Could not clean up the local usage log.");
        }
    }

    fn record(&mut self, row: Entry<'_>) {
        if self.usage.append(row).is_ok() {
            self.warned_on_log = false;
        } else if !self.warned_on_log {
            self.warned_on_log = true;
            self.status("Could not write the local usage log.");
        }
    }
}

struct Measurement {
    date: time::OffsetDateTime,
    time: Instant,
}
impl Measurement {
    fn new() -> Self {
        Self {
            date: local_now(),
            time: Instant::now(),
        }
    }
}
