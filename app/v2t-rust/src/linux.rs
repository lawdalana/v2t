mod audio;
mod background;
#[cfg(any(target_os = "macos", test))]
mod clipboard;
#[cfg(target_os = "macos")]
mod litellm;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
mod pipeline;
mod startup;

use std::{
    env,
    error::Error,
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use crate::{
    inference::{MIXED, SpeechProvider},
    models::LocalModels,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HELP: &str = "v2t for Linux and macOS

Usage:
  v2t transcribe <WAV> [speech options]
  v2t record [--seconds <1..300>] [speech options]
  v2t setup [--language <LANG>] [--correction]
  v2t settings
  v2t startup <enable|disable|status>
  v2t --background
  v2t stop-background
  v2t --help
  v2t --version

Speech options:
  --provider local|openai|openrouter  Transcription provider (default: local)
  --language th+en|th|en|auto        Speech language (default: th+en)
  --mai-backup                      Allow local failures to use MAI via OpenRouter with ZDR
  --correct                         Apply optional AI correction

WAV input: 16 kHz mono signed 16-bit PCM, up to five minutes.
Recording uses parec on Linux and ffmpeg on macOS (default: five seconds).
The transcript is written to stdout. Setup downloads the selected speech model,
or the local AI correction model with --correction. Cloud credentials come from
OPENAI_API_KEY or OPENROUTER_API_KEY for the selected provider.
";

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Help,
    Version,
    Settings,
    Startup(StartupAction),
    Background,
    StopBackground,
    Setup {
        language: Option<&'static str>,
        correction: bool,
    },
    #[cfg(target_os = "macos")]
    SetupLiteLlm {
        base_url: String,
        model: String,
        reasoning_effort: Option<String>,
    },
    Transcribe {
        path: PathBuf,
        options: SpeechOptions,
    },
    Record {
        seconds: u16,
        options: SpeechOptions,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum StartupAction {
    Enable,
    Disable,
    Status,
}

#[derive(Debug, PartialEq, Eq)]
struct SpeechOptions {
    provider: SpeechProvider,
    language: Option<&'static str>,
    mai_backup: bool,
    correct: CorrectionMode,
}

#[derive(Debug, PartialEq, Eq)]
enum CorrectionMode {
    Disabled,
    FollowDictionary,
    #[cfg(target_os = "macos")]
    Enabled,
    #[cfg(target_os = "macos")]
    LiteLlm,
}

impl CorrectionMode {
    fn enabled(&self, dictionary_enabled: bool) -> bool {
        match self {
            Self::Disabled => false,
            Self::FollowDictionary => dictionary_enabled,
            #[cfg(target_os = "macos")]
            Self::Enabled | Self::LiteLlm => true,
        }
    }
}

impl Default for SpeechOptions {
    fn default() -> Self {
        Self {
            provider: SpeechProvider::Local,
            language: Some(MIXED),
            mai_backup: false,
            correct: CorrectionMode::Disabled,
        }
    }
}

fn parse(args: impl IntoIterator<Item = OsString>) -> std::result::Result<Action, &'static str> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Ok(Action::Help);
    };
    let mode = match command.to_str() {
        Some("--help" | "-h" | "help") => return Ok(Action::Help),
        Some("--version" | "-V") if args.next().is_none() => return Ok(Action::Version),
        Some("settings") if args.next().is_none() => return Ok(Action::Settings),
        Some("--background") if args.next().is_none() => return Ok(Action::Background),
        Some("stop-background") if args.next().is_none() => return Ok(Action::StopBackground),
        #[cfg(target_os = "macos")]
        Some("setup-litellm") => {
            let usage = "Use v2t setup-litellm <HTTPS base URL> <model alias> [--reasoning-effort <level>] with the key on standard input.";
            let base_url = args
                .next()
                .and_then(|arg| arg.into_string().ok())
                .ok_or(usage)?;
            let model = args
                .next()
                .and_then(|arg| arg.into_string().ok())
                .ok_or(usage)?;
            let reasoning_effort = match args.next() {
                Some(flag) if flag == "--reasoning-effort" => Some(
                    args.next()
                        .and_then(|arg| arg.into_string().ok())
                        .ok_or(usage)?,
                ),
                Some(_) => return Err(usage),
                None => None,
            };
            if args.next().is_some() {
                return Err(usage);
            }
            return Ok(Action::SetupLiteLlm {
                base_url,
                model,
                reasoning_effort,
            });
        }
        Some("startup") => {
            let action = match args.next().as_deref().and_then(|value| value.to_str()) {
                Some("enable") => StartupAction::Enable,
                Some("disable") => StartupAction::Disable,
                Some("status") => StartupAction::Status,
                _ => return Err("Use v2t startup enable, disable, or status."),
            };
            if args.next().is_some() {
                return Err("Startup takes one argument.");
            }
            return Ok(Action::Startup(action));
        }
        Some(mode @ ("transcribe" | "record" | "setup")) => mode,
        _ => return Err("Unknown command. Use v2t --help for usage."),
    };
    let mut options = SpeechOptions::default();
    let mut path = None;
    let mut seconds = 5;
    let mut correction = false;
    let mut positional = false;
    while let Some(arg) = args.next() {
        match arg.to_str().filter(|_| !positional) {
            Some("--help" | "-h") => return Ok(Action::Help),
            Some("--") if mode == "transcribe" => positional = true,
            Some("--language") => {
                options.language = match args.next().as_deref().and_then(|s| s.to_str()) {
                    Some("th+en") => Some(MIXED),
                    Some("th") => Some("th"),
                    Some("en") => Some("en"),
                    Some("auto") => None,
                    _ => return Err("Choose language th+en, th, en, or auto."),
                };
            }
            Some("--provider") if mode != "setup" => {
                options.provider = match args.next().as_deref().and_then(|s| s.to_str()) {
                    Some("local") => SpeechProvider::Local,
                    Some("openai") => SpeechProvider::OpenAi,
                    Some("openrouter") => SpeechProvider::OpenRouter,
                    _ => return Err("Choose provider local, openai, or openrouter."),
                };
            }
            Some("--mai-backup") if mode != "setup" => options.mai_backup = true,
            Some("--correct") if mode != "setup" => {
                options.correct = CorrectionMode::FollowDictionary;
            }
            Some("--correction") if mode == "setup" => correction = true,
            Some("--seconds") if mode == "record" => {
                seconds = args
                    .next()
                    .as_deref()
                    .and_then(|s| s.to_str())
                    .and_then(|secs| secs.parse::<u16>().ok())
                    .filter(|secs| (1..=audio::MAX_SECONDS).contains(secs))
                    .ok_or("Recording duration must be from 1 to 300 seconds.")?;
            }
            _ if mode == "transcribe"
                && path.is_none()
                && (positional || !arg.as_encoded_bytes().starts_with(b"-")) =>
            {
                path = Some(PathBuf::from(arg));
            }
            _ => return Err("Unknown option or unexpected argument. Use v2t --help for usage."),
        }
    }
    if options.mai_backup && options.provider != SpeechProvider::Local {
        return Err("--mai-backup requires the local speech provider.");
    }
    match mode {
        "transcribe" => Ok(Action::Transcribe {
            path: path.ok_or("Specify the path to a WAV file.")?,
            options,
        }),
        "record" => Ok(Action::Record { seconds, options }),
        _ => Ok(Action::Setup {
            language: options.language,
            correction,
        }),
    }
}

struct Storage {
    data: PathBuf,
    config: PathBuf,
}

impl Storage {
    fn resolve() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        let subdir = |variable, fallback| {
            env::var_os(variable)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
                .map(|base| base.join("v2t"))
                .ok_or("Set HOME or the XDG configuration and data directories.")
        };
        #[cfg(target_os = "linux")]
        let (default_data, default_config) = (".local/share", ".config");
        #[cfg(target_os = "macos")]
        let (default_data, default_config) =
            ("Library/Application Support", "Library/Application Support");
        Ok(Self {
            data: subdir("XDG_DATA_HOME", default_data)?,
            config: subdir("XDG_CONFIG_HOME", default_config)?,
        })
    }
}

fn execute(action: Action) -> Result<()> {
    let output = |text: &str| writeln!(io::stdout().lock(), "{text}");
    match action {
        Action::Help => {
            output(HELP.trim_end())?;
            #[cfg(target_os = "macos")]
            output(
                "\nmacOS LiteLLM setup: v2t setup-litellm <HTTPS base URL> <model alias> [--reasoning-effort <level>] (key via stdin)",
            )?;
        }
        Action::Version => output(concat!("v2t ", env!("CARGO_PKG_VERSION")))?,
        Action::Settings => startup::edit_settings()?,
        Action::Startup(action) => {
            match action {
                StartupAction::Enable => startup::set_enabled(true)?,
                StartupAction::Disable => startup::set_enabled(false)?,
                StartupAction::Status => {}
            }
            output(if startup::enabled()? {
                "Start V2T At Startup: enabled."
            } else {
                "Start V2T At Startup: disabled."
            })?;
        }
        Action::Background => background::run(&Storage::resolve()?)?,
        Action::StopBackground => {
            let stopped = background::stop(&Storage::resolve()?.data)?;
            output(if stopped {
                "v2t background process stopped."
            } else {
                "v2t background process is not running."
            })?;
        }
        Action::Setup {
            language,
            correction,
        } => {
            let models = LocalModels::new(Storage::resolve()?.data)?;
            if correction {
                models.install_correction(|progress| eprintln!("{progress}"))?;
            } else {
                models.install_speech(language, |progress| eprintln!("{progress}"))?;
            }
            eprintln!("Model is ready.");
        }
        #[cfg(target_os = "macos")]
        Action::SetupLiteLlm {
            base_url,
            model,
            reasoning_effort,
        } => {
            let storage = Storage::resolve()?;
            litellm::setup(&storage.config, base_url, model, reasoning_effort)?;
            macos::enable_litellm(&storage.config)?;
            output("LiteLLM correction is enabled.")?;
        }
        Action::Transcribe { path, options } => {
            let wave = audio::read_wave(&path)?;
            if let Some(text) = pipeline::transcribe(&Storage::resolve()?, &options, &wave)? {
                output(&text)?;
            }
        }
        Action::Record { seconds, options } => {
            let storage = Storage::resolve()?;
            let (speech, transcript) = pipeline::start_recording(storage, options)?;
            eprintln!("Recording for {seconds} seconds…");
            if let Err(error) = audio::record(seconds, |wave| speech.send(wave)) {
                drop(speech);
                let _ = transcript.recv();
                return Err(error.into());
            }
            speech.finish();
            let transcript = transcript
                .recv()
                .map_err(|_| "The speech worker stopped unexpectedly.")??;
            if let Some(error) = transcript.partial_failure {
                eprintln!("Could not transcribe part of the recording. {error}");
            }
            match transcript.text {
                Some(text) => output(&text)?,
                None => eprintln!("No speech recognized."),
            }
        }
    }
    Ok(())
}

pub fn run() -> ExitCode {
    let args = env::args_os().skip(1);
    #[cfg(target_os = "macos")]
    if args.len() == 0
        && let Some(exit) = macos::run_from_bundle()
    {
        return exit;
    }
    let action = match parse(args) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("v2t: {error}");
            return ExitCode::from(2);
        }
    };
    match execute(action) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("v2t: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> std::result::Result<Action, &'static str> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn separates_startup_preference_from_worker_commands() {
        assert_eq!(command(&["settings"]), Ok(Action::Settings));
        assert_eq!(
            command(&["startup", "enable"]),
            Ok(Action::Startup(StartupAction::Enable))
        );
        assert_eq!(command(&["--background"]), Ok(Action::Background));
        assert_eq!(command(&["stop-background"]), Ok(Action::StopBackground));
        assert!(command(&["startup", "enable", "extra"]).is_err());
    }
    #[test]
    fn transcribe_defaults_to_offline_mixed_language() {
        assert_eq!(
            command(&["transcribe", "voice.wav"]),
            Ok(Action::Transcribe {
                path: PathBuf::from("voice.wav"),
                options: SpeechOptions {
                    provider: SpeechProvider::Local,
                    language: Some("th+en"),
                    mai_backup: false,
                    correct: CorrectionMode::Disabled
                },
            })
        );
    }

    #[test]
    fn requires_an_explicit_opt_in_for_mai_and_accepts_recording_budget() {
        assert_eq!(
            command(&[
                "record",
                "--seconds",
                "9",
                "--language",
                "en",
                "--mai-backup"
            ]),
            Ok(Action::Record {
                seconds: 9,
                options: SpeechOptions {
                    provider: SpeechProvider::Local,
                    language: Some("en"),
                    mai_backup: true,
                    correct: CorrectionMode::Disabled
                },
            })
        );
        assert!(
            command(&[
                "transcribe",
                "voice.wav",
                "--provider",
                "openai",
                "--mai-backup"
            ])
            .is_err()
        );
        assert!(command(&["record", "--seconds", "301"]).is_err());
    }

    #[test]
    fn command_parser_rejects_misplaced_options_and_accepts_dash_filenames() {
        assert!(command(&["setup", "--provider", "openai"]).is_err());
        assert!(command(&["transcribe", "a.wav", "b.wav"]).is_err());
        assert!(command(&["transcribe", "--"]).is_err());
        assert!(
            matches!(command(&["transcribe", "--", "-dash.wav"]), Ok(Action::Transcribe { path, .. }) if path.as_os_str() == "-dash.wav")
        );
    }
}
