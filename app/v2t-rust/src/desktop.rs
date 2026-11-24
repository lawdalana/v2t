use crate::{
    app::{self, Command, Dictation, Notice, Preferences, Transcript},
    capture::{self, Recorder},
    config,
    dialog::{self, Form, wide},
    inference::SpeechProvider,
    input::{self, FocusTarget, Keyboard},
    resource::{Reason, Routing},
    system::{Instance, Probe},
    windows_startup::Startup,
};
use dialog::{
    APPLY, DICT_INSTALL, DICT_LIST, DICT_RETURN, DICT_SAVE, DICTIONARY, INSTALL, START_AT_LOGIN,
};
use std::{
    cell::{Cell, RefCell},
    env, io,
    path::PathBuf,
    ptr::null_mut,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{Foundation::HWND, UI::WindowsAndMessaging::*},
    w,
};

mod window;
use window::{Tray, Window};

const WAKE: u32 = WM_APP + 2;
const COPY: usize = 200;
const EXIT: usize = 201;

thread_local! {
    static EVENTS: RefCell<Option<Sender<Event>>> = const { RefCell::new(None) };
}

enum Event {
    Start(FocusTarget),
    Release {
        canceled: bool,
        time: Instant,
    },
    CaptureReady {
        id: u64,
        result: Result<Recorder, &'static str>,
    },
    CaptureSegment {
        id: u64,
        audio: Vec<u8>,
    },
    CaptureStopped {
        id: u64,
        result: io::Result<()>,
    },
    RecordingUnavailable(&'static str),
    Engine(Notice),
    Action(u16),
    TrayMenu,
    TaskbarRestart,
    Tick,
    Resume,
    Hide,
    Exit,
}

pub fn run() -> io::Result<()> {
    let background = env::args_os().any(|arg| arg == "--background");
    let Some(_instance) = Instance::acquire()? else {
        if !background {
            startup_error(&io::Error::other(
                "v2t is already running. Open Settings from the Windows tray.",
            ));
        }
        return Ok(());
    };
    let dir = config::data_dir()?;
    let mut executable = env::current_exe()?;
    let startup = Startup::new(executable.clone());
    executable.pop();

    let (preferences, load_message) = Preferences::load(&dir, &executable);
    let (send, receive) = mpsc::channel();
    EVENTS.with(|slot| *slot.borrow_mut() = Some(send.clone()));
    let window = Window::create()?;
    let form = Form::create(window.0)?;
    let destination = window.0 as usize;
    let wake = send.clone();
    let commands = app::spawn(dir.clone(), move |notice| {
        enqueue(&wake, destination, Event::Engine(notice))
    })?;

    let accepts = Rc::new(Cell::new(true));
    let hook_busy = accepts.clone();
    let availability = Rc::new(Cell::new(recording_unavailable(&preferences, false)));
    let hook_availability = availability.clone();
    let released = send.clone();
    let started = send.clone();
    let keyboard = Keyboard::install(
        preferences.settings.shortcut,
        move |target| {
            if !hook_busy.get() || target.window == 0 || target.window as usize == destination {
                return false;
            }
            if let Some(message) = hook_availability.get() {
                enqueue(&started, destination, Event::RecordingUnavailable(message));
                return false;
            }
            let accepted = started.send(Event::Start(target)).is_ok();
            if accepted {
                hook_busy.set(false);
                post(destination);
            }
            accepted
        },
        move |canceled| {
            enqueue(
                &released,
                destination,
                Event::Release {
                    canceled,
                    time: Instant::now(),
                },
            )
        },
    )?;
    let mut desktop = Desktop {
        form,
        tray: Tray::create(window.0),
        keyboard,
        commands,
        events: send,
        accepts,
        availability,
        dir,
        executable,
        startup,
        preferences,
        recording: None,
        next_capture_id: 0,
        installing: false,
        warming: 0,
        retry_initial_warmup: true,
        available: false,
        speech_installed: false,
        correction_installed: false,
        last_text: None,
        probe: Probe::default(),
        routing: Routing::default(),
        last_maintenance: Instant::now(),
    };

    desktop.status(load_message.as_deref().unwrap_or("Starting v2t…"), false);
    let reason = desktop.resource_reason(input::capture_focus().window, true);
    if reason != Reason::None {
        desktop.status(&format!("MAI standby · {}", reason.label()), false);
    }
    desktop.warm(reason == Reason::None);
    if !background {
        desktop.show_settings();
    }

    // SAFETY: the structure is an out parameter for GetMessageW on the UI thread.
    let mut message = MSG::default();
    loop {
        // SAFETY: message points to writable storage and the thread owns the message queue.
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            break;
        }
        // SAFETY: Windows initialized the message, and window belongs to this thread.
        unsafe {
            if IsWindowVisible(window.0) == 0 || IsDialogMessageW(window.0, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        while let Ok(event) = receive.try_recv() {
            if desktop.event(event) {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn enqueue(sender: &Sender<Event>, window: usize, event: Event) {
    if sender.send(event).is_ok() {
        post(window);
    }
}

fn post(window: usize) {
    // SAFETY: only an opaque HWND token crosses threads. PostMessageW takes no shared pointer.
    unsafe {
        PostMessageW(window as HWND, WAKE, 0, 0);
    }
}

pub fn startup_error(error: &io::Error) {
    let message = wide(&error.to_string());
    // SAFETY: both strings are NUL-terminated and live through MessageBoxW.
    unsafe {
        MessageBoxW(
            null_mut(),
            message.as_ptr(),
            w!("v2t"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

struct Recording {
    id: u64,
    target: FocusTarget,
    recorder: Option<Recorder>,
    released: Option<Instant>,
    canceled: Arc<AtomicBool>,
    reason: Option<Reason>,
    stopped: bool,
    submitted: bool,
    pending: usize,
    transcript: Transcript,
    error: Option<String>,
}

struct Desktop {
    form: Form,
    tray: Tray,
    keyboard: Keyboard,
    commands: Sender<Command>,
    events: Sender<Event>,
    accepts: Rc<Cell<bool>>,
    availability: Rc<Cell<Option<&'static str>>>,
    dir: PathBuf,
    executable: PathBuf,
    startup: Startup,
    preferences: Preferences,
    recording: Option<Recording>,
    next_capture_id: u64,
    installing: bool,
    warming: usize,
    retry_initial_warmup: bool,
    available: bool,
    speech_installed: bool,
    correction_installed: bool,
    last_text: Option<String>,
    probe: Probe,
    routing: Routing,
    last_maintenance: Instant,
}

impl Desktop {
    fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Start(target) => self.start(target),
            Event::Release { canceled, time } => self.release(canceled, time),
            Event::CaptureReady { id, result } => self.capture_ready(id, result),
            Event::CaptureSegment { id, audio } => self.capture_segment(id, audio),
            Event::CaptureStopped { id, result } => self.capture_stopped(id, result),
            Event::RecordingUnavailable(message) => self.status(message, true),
            Event::Engine(notice) => self.notice(notice),
            Event::Action(command) => self.action(command),
            Event::TrayMenu => match self.tray.menu(self.last_text.is_some()) {
                x if x == EXIT => return true,
                x if x == COPY => self.copy(),
                x if x == DICTIONARY as usize => {
                    self.form.show_dictionary(&self.preferences.corrections)
                }
                x if x == APPLY as usize => self.show_settings(),
                _ => (),
            },
            Event::Tick => self.tick(),
            Event::TaskbarRestart => self.tray.add(),
            Event::Resume => {
                self.keyboard.reset();
                self.last_maintenance = Instant::now() - Duration::from_secs(60);
            }
            Event::Hide => {
                self.form.clear_secret_controls();
                // SAFETY: this UI thread owns the live window.
                unsafe {
                    ShowWindow(self.form.window, SW_HIDE);
                }
            }
            Event::Exit => return true,
        }
        false
    }

    fn start(&mut self, target: FocusTarget) {
        if self.recording.is_some() {
            return;
        }
        self.next_capture_id = self.next_capture_id.wrapping_add(1);
        let id = self.next_capture_id;
        self.recording = Some(Recording {
            id,
            target,
            recorder: None,
            released: None,
            canceled: Arc::new(AtomicBool::new(false)),
            reason: None,
            stopped: false,
            submitted: false,
            pending: 0,
            transcript: Transcript::default(),
            error: None,
        });
        let events = self.events.clone();
        let window = self.form.window as usize;
        if thread::Builder::new()
            .name("v2t-listening-setup".into())
            .spawn(move || {
                static CUE: std::sync::Mutex<()> = std::sync::Mutex::new(());
                let result = thread::scope(|scope| {
                    let tone = scope.spawn(|| {
                        let _guard = CUE
                            .lock()
                            .map_err(|_| io::Error::other("Previous listening cue failed."))?;
                        capture::play_cue()
                    });
                    let segments = events.clone();
                    let recorder = Recorder::prepare(move |audio| {
                        enqueue(&segments, window, Event::CaptureSegment { id, audio });
                    })
                    .map_err(|_| "Could not open the microphone. Check the Windows input device.");
                    if !matches!(tone.join(), Ok(Ok(()))) {
                        return Err(
                            "Could not play the listening cue. Check the audio output device.",
                        );
                    }
                    recorder
                });
                enqueue(&events, window, Event::CaptureReady { id, result });
            })
            .is_err()
        {
            self.recording.take();
            self.accepts.set(true);
            self.status("Could not prepare the listening cue.", true);
        }
    }

    fn capture_ready(&mut self, id: u64, result: Result<Recorder, &'static str>) {
        let Some(recording) = self
            .recording
            .as_mut()
            .filter(|recording| recording.id == id)
        else {
            return;
        };
        let mut recorder = match result {
            Ok(recorder) => recorder,
            Err(message) => {
                self.recording.take();
                self.accepts.set(true);
                self.status(message, true);
                return;
            }
        };
        if self.keyboard.keys_released() {
            self.recording.take();
            self.accepts.set(true);
            return;
        }
        if !recording.target.matches(input::capture_focus()) {
            self.recording.take();
            self.accepts.set(true);
            self.status(
                "Keep the cursor in the same text field while starting dictation.",
                true,
            );
            return;
        }
        if recorder.start().is_err() {
            self.recording.take();
            self.accepts.set(true);
            self.status("Could not start microphone capture.", true);
            return;
        }
        recording.recorder = Some(recorder);
        self.status("Listening…", false);
    }

    fn release(&mut self, canceled: bool, time: Instant) {
        let Some(recording) = self.recording.as_mut().filter(|r| r.released.is_none()) else {
            return;
        };
        let Some(recorder) = recording.recorder.take() else {
            self.recording.take();
            self.accepts.set(true);
            self.ready();
            return;
        };
        recording.released = Some(time);
        recording.canceled.store(canceled, Ordering::Release);
        let id = recording.id;
        let events = self.events.clone();
        let window = self.form.window as usize;
        let stopping = thread::Builder::new()
            .name("v2t-finish-capture".into())
            .spawn(move || {
                let result = recorder.stop();
                enqueue(&events, window, Event::CaptureStopped { id, result });
            });
        if let Err(error) = stopping {
            enqueue(
                &self.events,
                window,
                Event::CaptureStopped {
                    id,
                    result: Err(error),
                },
            );
        } else if !canceled && recording.pending > 0 {
            self.status("Transcribing…", false);
        }
    }

    fn capture_segment(&mut self, id: u64, audio: Vec<u8>) {
        let Some(recording) = self
            .recording
            .as_ref()
            .filter(|recording| recording.id == id && !recording.canceled.load(Ordering::Acquire))
        else {
            return;
        };
        // Make one routing decision per recording so GPU work from an earlier
        // segment cannot route the rest of the recording to the cloud.
        let reason = recording.reason.unwrap_or_else(|| {
            self.resource_reason(input::capture_focus().window, self.warming == 0)
        });
        if self.preferences.settings.provider == SpeechProvider::Local && reason == Reason::None {
            self.retry_initial_warmup = false;
        }
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        recording.reason = Some(reason);
        let command = Command::Dictate(Box::new(Dictation {
            id,
            canceled: Arc::clone(&recording.canceled),
            preferences: self.preferences.clone(),
            audio,
            reason,
        }));
        if self.commands.send(command).is_err() {
            recording
                .error
                .get_or_insert_with(|| "The speech worker is unavailable. Restart v2t.".into());
        } else {
            recording.submitted = true;
            recording.pending += 1;
            if recording.released.is_some() {
                self.status("Transcribing…", false);
            }
        }
    }

    fn capture_stopped(&mut self, id: u64, result: io::Result<()>) {
        let Some(recording) = self.recording.as_mut().filter(|r| r.id == id) else {
            return;
        };
        recording.stopped = true;
        if result.is_err() {
            recording
                .error
                .get_or_insert_with(|| "Could not finish microphone capture.".into());
        }
        self.finish_dictation();
    }

    fn segment_finished(&mut self, id: u64, result: Result<Transcript, String>) {
        let Some(recording) = self.recording.as_mut().filter(|r| r.id == id) else {
            return;
        };
        recording.pending -= 1;
        match result {
            Ok(segment) => recording.transcript.append(segment),
            Err(error) => {
                recording.error.get_or_insert(error);
            }
        }
        self.finish_dictation();
    }

    fn finish_dictation(&mut self) {
        if self
            .recording
            .as_ref()
            .is_none_or(|r| !r.stopped || (r.pending > 0 && !r.canceled.load(Ordering::Acquire)))
        {
            return;
        }
        let Some(recording) = self.recording.take() else {
            return;
        };
        self.accepts.set(true);
        if recording.canceled.load(Ordering::Acquire) {
            self.ready();
            return;
        }

        let result = recording.transcript;
        if result.text.is_empty() {
            if let Some(error) = recording.error {
                self.status(&error, true);
            } else if recording.submitted {
                self.status(
                    "No speech recognized. Hold the shortcut to try again.",
                    false,
                );
            } else {
                self.ready();
            }
            return;
        }
        let inserted = input::try_insert(recording.target, &result.text);
        self.last_text = Some(result.text);
        if let Some(error) = recording.error {
            let status = if inserted {
                "Typed partial transcript."
            } else {
                "Use Copy last transcript in the v2t tray for the partial transcript."
            };
            self.status(&format!("{status} {error}"), true);
        } else if inserted {
            let verb = if result.backup {
                "MAI backup typed"
            } else {
                "Typed"
            };
            let suffix = if result.correction_fallback {
                " Used dictionary result."
            } else {
                ""
            };
            let elapsed = recording
                .released
                .map_or(Duration::ZERO, |time| time.elapsed());
            self.status(
                &format!("{verb} in {:.2} s.{suffix}", elapsed.as_secs_f64()),
                false,
            );
        } else {
            self.status(
                "Text is ready. Use Copy last transcript in the v2t tray.",
                true,
            );
        }
    }

    fn notice(&mut self, notice: Notice) {
        match notice {
            Notice::Status(status) => {
                if self.recording.is_none() {
                    self.status(&status, false);
                }
            }
            Notice::Unavailable(error) => {
                if let Some(recording) = &mut self.recording {
                    recording.pending = 0;
                    recording.error.get_or_insert(error);
                    self.finish_dictation();
                } else {
                    self.status(&error, true);
                }
            }
            Notice::Models {
                speech,
                preferred,
                correction,
            } => {
                self.available = speech;
                self.availability
                    .set(recording_unavailable(&self.preferences, self.available));
                self.speech_installed = preferred;
                self.correction_installed = correction;
                self.update_model_buttons();
            }
            Notice::Warmed => self.warming = self.warming.saturating_sub(1),
            Notice::Installed(result) => {
                self.installing = false;
                self.update_model_buttons();
                match result {
                    Ok(()) => {
                        let allow = self.resource_reason_now();
                        self.warm(allow);
                        if self.recording.is_none() {
                            self.status("Model installed.", false);
                        }
                    }
                    Err(message) => self.status(&message, true),
                }
            }
            Notice::SegmentFinished { id, result } => self.segment_finished(id, result),
        }
    }

    fn action(&mut self, command: u16) {
        let result = match command {
            APPLY => self.apply(),
            DICTIONARY => {
                self.form.show_dictionary(&self.preferences.corrections);
                Ok(())
            }
            DICT_RETURN => {
                self.show_settings();
                Ok(())
            }
            START_AT_LOGIN => self.change_startup(),
            DICT_SAVE => self.save_dictionary(),
            DICT_LIST => {
                self.form.row_selected();
                Ok(())
            }
            dialog::DICT_ADD | dialog::DICT_UPDATE | dialog::DICT_REMOVE => {
                self.form.edit_dictionary(command)
            }
            INSTALL | DICT_INSTALL => self.install(command == DICT_INSTALL),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.status(&error.to_string(), true);
        }
    }

    fn show_settings(&self) {
        if let Err(error) = self.refresh_startup() {
            self.status(&format!("Could not check Windows Startup. {error}"), false);
        }
        self.form.show_settings(&self.preferences.settings);
    }

    fn refresh_startup(&self) -> io::Result<()> {
        let enabled = self.startup.enabled();
        self.form.set_start_at_login(enabled.as_ref().ok().copied());
        enabled.map(|_| ())
    }

    fn change_startup(&self) -> io::Result<()> {
        let enabled = self.form.starts_at_login();
        let changed = self.startup.set_enabled(enabled);
        let refreshed = self.refresh_startup();
        changed?;
        refreshed?;
        self.status(
            if enabled {
                "v2t will start in the background when you sign in."
            } else {
                "v2t will stay closed when you sign in."
            },
            false,
        );
        Ok(())
    }

    fn apply(&mut self) -> io::Result<()> {
        if self.recording.is_some() || self.installing {
            return Err(config::invalid(
                "Finish the current operation before saving settings.",
            ));
        }
        let settings = self.form.settings()?;
        settings.save(&self.dir)?;
        self.keyboard.set_binding(settings.shortcut)?;
        self.preferences.router_key = settings.router_key(&self.executable);
        self.preferences.settings = settings;
        self.availability
            .set(recording_unavailable(&self.preferences, self.available));
        self.routing = Routing::default();
        let allow_warmup = self.resource_reason_now();
        self.warm(allow_warmup);
        self.status("Settings saved.", false);
        Ok(())
    }

    fn save_dictionary(&mut self) -> io::Result<()> {
        if self.recording.is_some() || self.installing {
            return Err(config::invalid(
                "Finish the current operation before saving the dictionary.",
            ));
        }
        let corrections = self.form.corrections();
        corrections.save(&self.dir)?;
        self.preferences.corrections = corrections;
        let allow_warmup = self.resource_reason_now();
        self.warm(allow_warmup);
        self.status("Dictionary saved.", false);
        Ok(())
    }

    fn resource_reason_now(&mut self) -> bool {
        self.resource_reason(input::capture_focus().window, self.warming == 0) == Reason::None
    }

    fn install(&mut self, correction: bool) -> io::Result<()> {
        if self.recording.is_some() || self.installing {
            return Err(config::invalid(
                "Finish the current operation before installing a model.",
            ));
        }
        let language = if correction {
            None
        } else {
            self.form.language()?
        };
        let events = self.events.clone();
        let window = self.form.window as usize;
        app::install(self.dir.clone(), language, correction, move |notice| {
            enqueue(&events, window, Event::Engine(notice))
        })?;
        self.installing = true;
        self.update_model_buttons();
        self.status("Preparing model setup…", false);
        Ok(())
    }

    fn warm(&mut self, allow: bool) {
        if self
            .commands
            .send(Command::Warm(Box::new(self.preferences.clone()), allow))
            .is_ok()
        {
            self.warming += 1;
        }
    }

    fn tick(&mut self) {
        if self.last_maintenance.elapsed() >= Duration::from_secs(60) {
            self.last_maintenance = Instant::now();
            let _ = self.commands.send(Command::Maintenance);
        }
        if self.recording.is_some() || self.installing || !self.preferences.can_backup() {
            return;
        }
        let before = self.routing.current;
        let reason = self.resource_reason(input::capture_focus().window, self.warming == 0);
        if reason != before {
            if reason != Reason::None {
                let _ = self.commands.send(Command::Suspend);
                self.status(&format!("MAI standby · {}", reason.label()), false);
            } else if self.retry_initial_warmup {
                self.retry_initial_warmup = false;
                self.warm(true);
            } else {
                self.status("Local will resume on the next dictation.", false);
            }
        }
    }

    fn resource_reason(&mut self, window: isize, observe_gpu: bool) -> Reason {
        if !self.preferences.can_backup() {
            return Reason::None;
        }
        self.routing.update(self.probe.capture(window, observe_gpu))
    }

    fn update_model_buttons(&self) {
        self.form.installation_buttons(
            self.installing,
            self.speech_installed,
            self.correction_installed,
        );
    }

    fn ready(&self) {
        self.status(
            &format!(
                "Ready. Hold {} to speak.",
                self.preferences.settings.shortcut.label()
            ),
            false,
        );
    }

    fn copy(&self) {
        if let Some(text) = &self.last_text {
            if input::copy_text(self.form.window as isize, text).is_ok() {
                self.status("Transcript copied.", false);
            } else {
                self.status("Clipboard is busy. Try copying again.", true);
            }
        }
    }

    fn status(&self, message: &str, notify: bool) {
        self.form.status(message);
        self.tray.status(message, notify);
    }
}

fn recording_unavailable(preferences: &Preferences, local_available: bool) -> Option<&'static str> {
    match preferences.settings.provider {
        SpeechProvider::Local if !local_available && !preferences.can_backup() => {
            Some("Install the local speech model in v2t Settings.")
        }
        SpeechProvider::OpenAi if preferences.key().is_none() => {
            Some("Add your OpenAI API key in Settings.")
        }
        SpeechProvider::OpenRouter if preferences.key().is_none() => {
            Some("Add your OpenRouter API key in Settings.")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcut::Binding;

    #[test]
    fn native_settings_controls_round_trip_without_opening_the_microphone() {
        let window = Window::create().unwrap();
        let form = Form::create(window.0).unwrap();
        let mut settings = config::Settings {
            provider: SpeechProvider::OpenRouter,
            language: Some("th".into()),
            shortcut: Binding::parse("Alt+F9").unwrap(),
            enable_mai_backup: false,
            ..Default::default()
        };
        settings.open_ai_key.push_str("synthetic-api-fixture");
        form.fill_settings(&settings);
        let result = form.settings().unwrap();
        assert_eq!(result.provider, SpeechProvider::OpenRouter);
        assert_eq!(result.language.as_deref(), Some("th"));
        assert_eq!(result.shortcut, settings.shortcut);
        assert!(!result.enable_mai_backup);
        assert_eq!(result.open_ai_key.as_str(), "synthetic-api-fixture");
        form.clear_secret_controls();
        assert!(form.settings().unwrap().open_ai_key.is_empty());
        form.set_start_at_login(Some(true));
        assert!(form.starts_at_login());
        form.set_start_at_login(Some(false));
        assert!(!form.starts_at_login());
    }
}
