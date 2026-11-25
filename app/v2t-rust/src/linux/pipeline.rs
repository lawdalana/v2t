use std::{
    env, fs, io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

use super::{Result, SpeechOptions, Storage, audio, background};
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

pub(super) type RecordingResult = std::result::Result<RecordingTranscript, String>;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RecordingTranscript {
    pub text: Option<String>,
    pub partial_failure: Option<String>,
    pub correction_fallback: bool,
}

struct Segment {
    text: String,
    correction_fallback: bool,
}

pub(super) struct RecordingSpeech {
    sender: mpsc::Sender<Vec<u8>>,
    canceled: Arc<AtomicBool>,
    finishing: bool,
}

impl RecordingSpeech {
    pub(super) fn send(&self, wave: Vec<u8>) -> io::Result<()> {
        self.sender
            .send(wave)
            .map_err(|_| io::Error::other("The speech worker stopped unexpectedly."))
    }

    pub(super) fn finish(mut self) {
        self.finishing = true;
    }
}

impl Drop for RecordingSpeech {
    fn drop(&mut self) {
        if !self.finishing {
            self.canceled.store(true, Ordering::Release);
        }
    }
}

pub(super) fn start_recording(
    storage: Storage,
    options: SpeechOptions,
) -> io::Result<(RecordingSpeech, mpsc::Receiver<RecordingResult>)> {
    let credentials =
        resolve_credentials(&options).map_err(|error| io::Error::other(error.to_string()))?;
    let mut session = None;
    spawn_recording(move |wave| {
        match session.get_or_insert_with(|| {
            Transcriber::new(&storage, &options).map_err(|error| error.to_string())
        }) {
            Ok(transcriber) => {
                transcribe_segment(transcriber, &storage, &options, &credentials, wave)
                    .map_err(|error| error.to_string())
            }
            Err(error) => Err(error.clone()),
        }
    })
}

fn spawn_recording(
    mut transcribe: impl FnMut(&[u8]) -> std::result::Result<Option<Segment>, String> + Send + 'static,
) -> io::Result<(RecordingSpeech, mpsc::Receiver<RecordingResult>)> {
    let (sender, chunks) = mpsc::channel::<Vec<u8>>();
    let (result, receiver) = mpsc::sync_channel(1);
    let canceled = Arc::new(AtomicBool::new(false));
    let cancellation = canceled.clone();
    thread::Builder::new()
        .name("v2t-recording-speech".into())
        .spawn(move || {
            let mut transcript = String::new();
            let mut first_error = None;
            let mut correction_fallback = false;
            for wave in chunks {
                if cancellation.load(Ordering::Acquire) {
                    break;
                }
                match transcribe(&wave) {
                    Ok(Some(segment)) => {
                        text::append_transcript(&mut transcript, &segment.text);
                        correction_fallback |= segment.correction_fallback;
                    }
                    Ok(None) => (),
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
            // Closing the model processes comes before notifying a CLI that may exit.
            drop(transcribe);
            if !cancellation.load(Ordering::Acquire) {
                let text = (!transcript.is_empty()).then_some(transcript);
                let outcome = match (text, first_error) {
                    (None, Some(error)) => Err(error),
                    (text, partial_failure) => Ok(RecordingTranscript {
                        text,
                        partial_failure,
                        correction_fallback,
                    }),
                };
                let _ = result.send(outcome);
            }
        })?;
    Ok((
        RecordingSpeech {
            sender,
            canceled,
            finishing: false,
        },
        receiver,
    ))
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
    let mut session = Transcriber::new(storage, options)?;
    Ok(
        transcribe_segment(&mut session, storage, options, &credentials, wave)?
            .map(|segment| segment.text),
    )
}

struct Transcriber {
    corrections: Corrections,
    models: LocalModels,
    log: Log,
    #[cfg(target_os = "macos")]
    litellm: Option<std::result::Result<super::litellm::Connection, String>>,
}

impl Transcriber {
    fn new(storage: &Storage, _options: &SpeechOptions) -> Result<Self> {
        let corrections = Corrections::load(&storage.config)?;
        let models = LocalModels::new(storage.data.clone())?;
        let mut log = Log::new(storage.data.clone());
        if with_log_lock(&storage.data, || log.cleanup()).is_err() {
            eprintln!("Could not clean up the local usage log.");
        }
        #[cfg(target_os = "macos")]
        let litellm = {
            use super::CorrectionMode;
            let use_litellm = match _options.correct {
                CorrectionMode::LiteLlm => true,
                CorrectionMode::FollowDictionary => {
                    corrections.enable_ai && super::macos::uses_litellm(&storage.config)
                }
                _ => false,
            };
            use_litellm.then(|| {
                super::litellm::Connection::load(&storage.config).map_err(|error| error.to_string())
            })
        };
        Ok(Self {
            corrections,
            models,
            log,
            #[cfg(target_os = "macos")]
            litellm,
        })
    }
}

fn transcribe_segment(
    session: &mut Transcriber,
    storage: &Storage,
    options: &SpeechOptions,
    credentials: &Credentials,
    wave: &[u8],
) -> Result<Option<Segment>> {
    let Transcriber {
        corrections,
        models,
        log,
        #[cfg(target_os = "macos")]
        litellm,
    } = session;
    let speech = recognize(wave, models, options, credentials, &storage.data, log)?;
    let (mapped, protected) = corrections.apply(&text::for_insertion(&speech.text));
    if mapped.is_empty() {
        eprintln!("No speech was recognized.");
        return Ok(None);
    }
    let mut segment = Segment {
        text: mapped,
        correction_fallback: false,
    };
    if !options.correct.enabled(corrections.enable_ai) {
        return Ok(Some(segment));
    }
    segment.correction_fallback = true;
    let started = local_now();
    let elapsed = Instant::now();
    let mut local = || {
        if !models.correction_available() {
            eprintln!("Local AI correction is unavailable. Using dictionary replacements.");
            return None;
        }
        let result =
            match background::correct(&storage.data, &segment.text, &protected, CORRECTION_TIMEOUT)
            {
                None | Some(Err(InferenceError::Unavailable(_))) => {
                    eprintln!("Loading the local AI correction model…");
                    models
                        .warm_correction(background::CORRECTION_STARTUP_TIMEOUT)
                        .and_then(|()| {
                            eprintln!("Correcting text with local AI…");
                            models.correct(&segment.text, &protected, CORRECTION_TIMEOUT)
                        })
                }
                Some(result) => result,
            };
        Some(("Local", CORRECTION_MODEL_FILE, result))
    };
    #[cfg(target_os = "macos")]
    let attempt = match litellm {
        Some(Ok(litellm)) => {
            eprintln!("Correcting text with LiteLLM…");
            Some((
                "LiteLLM",
                litellm.model(),
                litellm.correct(&segment.text, &protected),
            ))
        }
        Some(Err(error)) => {
            eprintln!("{error} Using dictionary replacements.");
            None
        }
        None => local(),
    };
    #[cfg(not(target_os = "macos"))]
    let attempt = local();
    let Some((provider, model, result)) = attempt else {
        return Ok(Some(segment));
    };
    let outcome = match &result {
        Ok(Some(_)) => "ok",
        Ok(None) => "fallback",
        Err(InferenceError::Timeout(_)) => "canceled",
        Err(_) => "failed",
    };
    save_entry(
        &storage.data,
        log,
        Entry {
            started_at: started,
            stage: "correction",
            provider,
            model,
            outcome,
            audio_seconds: None,
            elapsed: elapsed.elapsed().as_secs_f64(),
            billed_seconds: None,
            cost: (provider == "Local").then_some(0.0),
            cost_kind: if provider == "Local" {
                "local"
            } else {
                "unknown"
            },
        },
    );
    if let Ok(Some(corrected)) = result {
        segment.text = text::for_insertion(&corrected);
        segment.correction_fallback = false;
    } else {
        if let Err(error) = result {
            eprintln!("{error}");
        }
        eprintln!("AI correction did not complete. Using dictionary replacements.");
    }
    Ok(Some(segment))
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
    // The socket worker requires three voiced frames. Capture can also produce
    // a short voiced continuation immediately after a forced cut.
    let background_attempt = if speech_gate::has_speech(wave) {
        background::speech(data, wave, options.language, local_budget)
    } else {
        None
    };
    let (model, result) = background_attempt.unwrap_or_else(|| {
        let result = models.speech(wave, options.language, local_budget);
        let model = models.loaded_speech().map(|model| model.file.to_owned());
        (model, result)
    });
    let model = model.as_deref().unwrap_or("Local Whisper");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{TryRecvError, channel};

    #[test]
    fn processes_audio_before_finish_and_collects_successful_chunks_in_order() {
        let (processed, calls) = channel();
        let (recording, transcript) = spawn_recording(move |wave| {
            processed.send(wave[0]).unwrap();
            match wave[0] {
                1 => Ok(Some(Segment {
                    text: "first".into(),
                    correction_fallback: false,
                })),
                2 => Err("test inference failure".into()),
                3 => Ok(Some(Segment {
                    text: "third".into(),
                    correction_fallback: true,
                })),
                _ => unreachable!(),
            }
        })
        .unwrap();

        recording.send(vec![1]).unwrap();
        assert_eq!(calls.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        assert_eq!(transcript.try_recv(), Err(TryRecvError::Empty));

        recording.send(vec![2]).unwrap();
        recording.send(vec![3]).unwrap();
        recording.finish();
        assert_eq!(
            transcript.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok(RecordingTranscript {
                text: Some("first third".into()),
                partial_failure: Some("test inference failure".into()),
                correction_fallback: true,
            })
        );
    }

    #[test]
    fn cancels_queued_transcription_when_recording_is_dropped() {
        let (processed, calls) = channel();
        let (resume, waiting) = channel();
        let (recording, transcript) = spawn_recording(move |wave| {
            processed.send(wave[0]).unwrap();
            if wave[0] == 1 {
                waiting.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            Ok(None)
        })
        .unwrap();

        recording.send(vec![1]).unwrap();
        assert_eq!(calls.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        recording.send(vec![2]).unwrap();
        drop(recording);
        resume.send(()).unwrap();
        assert!(transcript.recv_timeout(Duration::from_secs(2)).is_err());
        assert_eq!(calls.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn returns_the_error_when_no_chunk_has_a_transcript() {
        let (recording, transcript) =
            spawn_recording(|_| Err("test inference failure".into())).unwrap();
        recording.send(vec![1]).unwrap();
        recording.finish();
        assert_eq!(
            transcript.recv_timeout(Duration::from_secs(2)).unwrap(),
            Err("test inference failure".into())
        );
    }

    #[test]
    fn releases_the_model_process_before_delivering_the_transcript() {
        struct ModelProcess {
            stopped: mpsc::Sender<()>,
            reaped: mpsc::Receiver<()>,
        }
        impl Drop for ModelProcess {
            fn drop(&mut self) {
                let _ = self.stopped.send(());
                let _ = self.reaped.recv_timeout(Duration::from_secs(2));
            }
        }

        let (stop, stopped) = channel();
        let (reap, reaped) = channel();
        let model = ModelProcess {
            stopped: stop,
            reaped,
        };
        let (recording, transcript) = spawn_recording(move |_| {
            let _model = &model;
            Ok(Some(Segment {
                text: "ready".into(),
                correction_fallback: false,
            }))
        })
        .unwrap();
        recording.send(vec![1]).unwrap();
        recording.finish();

        stopped.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(transcript.try_recv(), Err(TryRecvError::Empty));
        reap.send(()).unwrap();
        assert_eq!(
            transcript.recv_timeout(Duration::from_secs(2)).unwrap(),
            Ok(RecordingTranscript {
                text: Some("ready".into()),
                partial_failure: None,
                correction_fallback: false,
            })
        );
    }
}
