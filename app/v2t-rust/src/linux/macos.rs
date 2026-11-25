mod dictionary;
mod hotkey;
mod launcher;
mod modifiers;
mod preferences;
mod recording;
mod right_option;
mod window;

use std::{
    cell::{Cell, RefCell},
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::mpsc,
    thread,
};

use block2::RcBlock;
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::{Retained, Weak, autoreleasepool},
    runtime::{Bool, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSApplicationDelegateReply, NSApplicationTerminateReply,
    NSBeep, NSMenu, NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSSound, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength, NSWindowDelegate,
};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{
    NSArray, NSBundle, NSNotification, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes,
    NSString, NSTimer,
};

use super::{
    Storage,
    clipboard::{CopyOutcome, copy_transcript},
    pipeline::{self, RecordingResult},
};
use crate::models::LocalModels;
use dictionary::DictionaryEditor;
use hotkey::{Edge, Hotkey, Registration};
use modifiers::ModifierShortcut;
use preferences::{CorrectionProvider, Preferences, Shortcut};
use recording::{PendingTranscript, Recording};
use right_option::RightOptionShortcut;
use window::MainWindow;

const READY: &str = "Ready to record";
const MIC_SETTINGS: &str = "Allow v2t under Microphone in your Mac’s privacy settings";
const RIGHT_OPTION_FOCUSED: &str =
    "Right Option + Space works in this window. Allow Accessibility to use it in other apps.";

pub(super) fn correction_enabled(config: &Path) -> bool {
    Preferences::load(config).is_ok_and(|preferences| preferences.ai_correction)
}

pub(super) fn uses_litellm(config: &Path) -> bool {
    Preferences::load(config)
        .is_ok_and(|preferences| preferences.correction_provider == CorrectionProvider::LiteLlm)
}

pub(super) fn enable_litellm(config: &Path) -> std::io::Result<()> {
    let mut preferences = Preferences::load(config)?;
    preferences.ai_correction = true;
    preferences.correction_provider = CorrectionProvider::LiteLlm;
    preferences.save(config)
}

pub(super) fn run_from_bundle() -> Option<ExitCode> {
    let id = autoreleasepool(|_| {
        NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|id| id.to_string())
    });
    match id.as_deref() {
        Some(launcher::APP_ID) => Some(run()),
        Some(launcher::LAUNCHER_ID) => Some(launcher::run()),
        _ => None,
    }
}

fn run() -> ExitCode {
    let Some(main) = MainThreadMarker::new() else {
        eprintln!("v2t: The desktop app must run on the main thread.");
        return ExitCode::FAILURE;
    };
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        let controller = Controller::new(main);
        app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        app.setMainMenu(Some(&controller.ivars().menu.main_menu));
        app.run();
        app.setDelegate(None);
        controller.ivars().window.window.setDelegate(None);
        controller.stop_polling();
    });
    ExitCode::SUCCESS
}

enum State {
    Idle,
    RequestingPermission(RecordOrigin, mpsc::Receiver<Update>),
    Recording(RecordOrigin, Recording),
    Transcribing(PendingTranscript),
    Working(mpsc::Receiver<Update>),
}

#[derive(Clone, Copy)]
enum RecordOrigin {
    Button,
    Shortcut(Shortcut),
}

impl RecordOrigin {
    fn shortcut(self) -> Option<Shortcut> {
        match self {
            Self::Button => None,
            Self::Shortcut(shortcut) => Some(shortcut),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ShortcutAction {
    Start,
    Stop,
}

fn shortcut_action(state: &State, shortcut: Shortcut, edge: Edge) -> Option<ShortcutAction> {
    match (state, edge) {
        (State::Idle, Edge::Pressed) => Some(ShortcutAction::Start),
        (State::Recording(origin, _), Edge::Released) if origin.shortcut() == Some(shortcut) => {
            Some(ShortcutAction::Stop)
        }
        _ => None,
    }
}

enum Update {
    Microphone(bool),
    Transcript(RecordingResult),
    ModelProgress(String),
    Model(Result<(), String>, &'static str),
}

enum Model {
    Speech,
    Correction,
}

#[derive(Clone, Copy)]
enum LaunchState {
    Starting,
    RecordRequested,
    Ready,
}

struct Menu {
    bar_item: Retained<NSStatusItem>,
    status: Retained<NSMenuItem>,
    open: Retained<NSMenuItem>,
    dictionary: Retained<NSMenuItem>,
    record: Retained<NSMenuItem>,
    copy_last: Retained<NSMenuItem>,
    install: Retained<NSMenuItem>,
    quit: Retained<NSMenuItem>,
    main_menu: Retained<NSMenu>,
    app_quit: Retained<NSMenuItem>,
}

struct ControllerIvars {
    menu: Menu,
    window: MainWindow,
    dictionary: Retained<DictionaryEditor>,
    completion_sound: Option<Retained<NSSound>>,
    preferences: Cell<Preferences>,
    shortcuts: RefCell<ShortcutListeners>,
    state: RefCell<State>,
    launch_state: Cell<LaunchState>,
    timer: RefCell<Option<Retained<NSTimer>>>,
}

#[derive(Default)]
struct ShortcutListeners {
    carbon: Vec<Hotkey>,
    right_command_option: Option<ModifierShortcut>,
    right_option_space: Option<RightOptionShortcut>,
}

impl ShortcutListeners {
    fn right_option_is_focused(&self) -> bool {
        self.right_option_space
            .as_ref()
            .is_some_and(|shortcut| !shortcut.is_global())
    }
}

fn prepare_registration<T>(
    registration: io::Result<Option<T>>,
    save: bool,
    startup_error: &mut Option<io::Error>,
) -> io::Result<Option<T>> {
    match registration {
        Err(error) if !save => {
            startup_error.get_or_insert(error);
            Ok(None)
        }
        result => result,
    }
}

define_class!(
    // SAFETY: NSObject has no additional subclassing requirements. AppKit invokes
    // the menu actions and the timer on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    #[allow(non_snake_case)] // Method names match the AppKit delegate protocol.
    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn applicationDidFinishLaunching(&self, _notification: &NSNotification) {
            self.present_window();
            match self.configure_shortcuts(self.ivars().preferences.get(), false) {
                Err(error) => {
                    eprintln!("v2t: {error}");
                    self.set_state(State::Idle, &error.to_string());
                }
                Ok(()) if self.ivars().shortcuts.borrow().right_option_is_focused() => {
                    self.set_state(State::Idle, RIGHT_OPTION_FOCUSED);
                }
                Ok(()) => {}
            }
            if matches!(self.ivars().launch_state.replace(LaunchState::Ready), LaunchState::RecordRequested) {
                self.start_from_launcher();
            }
        }

        #[unsafe(method(application:openFiles:))]
        fn application_openFiles(&self, app: &NSApplication, files: &NSArray<NSString>) {
            let bundle = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
            let requested = files.to_vec();
            if let [file] = requested.as_slice()
                && launcher::is_record_request(&bundle, Path::new(&file.to_string()))
            {
                app.replyToOpenOrPrint(NSApplicationDelegateReply::Success);
                match self.ivars().launch_state.get() {
                    LaunchState::Ready => self.start_from_launcher(),
                    _ => self.ivars().launch_state.set(LaunchState::RecordRequested),
                }
            } else {
                app.replyToOpenOrPrint(NSApplicationDelegateReply::Failure);
            }
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn applicationShouldHandleReopen_hasVisibleWindows(
            &self,
            _app: &NSApplication,
            _has_visible_windows: bool,
        ) -> bool {
            self.present_window();
            false
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn applicationShouldTerminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            if matches!(*self.ivars().state.borrow(), State::Transcribing(_) | State::Working(_)) {
                return NSApplicationTerminateReply::TerminateCancel;
            }
            self.set_state(State::Idle, READY);
            NSApplicationTerminateReply::TerminateNow
        }

        #[unsafe(method(applicationDidResignActive:))]
        fn applicationDidResignActive(&self, _notification: &NSNotification) {
            self.cancel_focused_shortcut();
        }

        #[unsafe(method(applicationDidBecomeActive:))]
        fn applicationDidBecomeActive(&self, _notification: &NSNotification) {
            self.activate_global_right_option();
        }
    }

    #[allow(non_snake_case)] // Method name matches the AppKit delegate protocol.
    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowDidResignKey:))]
        fn windowDidResignKey(&self, _notification: &NSNotification) {
            self.cancel_focused_shortcut();
        }
    }

    impl Controller {
        #[unsafe(method(showWindow:))]
        fn show_window(&self, _sender: &NSMenuItem) {
            self.present_window();
        }

        #[unsafe(method(showDictionary:))]
        fn show_dictionary(&self, _sender: &NSObject) {
            self.ivars().dictionary.show();
        }

        #[unsafe(method(toggleRecording:))]
        fn toggle_recording(&self, _sender: &NSObject) {
            let state = self.ivars().state.borrow();
            match &*state {
                State::Idle => {
                    drop(state);
                    self.request_microphone(RecordOrigin::Button);
                }
                State::Recording(_, _) => {
                    drop(state);
                    self.finish_recording();
                }
                _ => {}
            }
        }

        #[unsafe(method(installModel:))]
        fn install_model(&self, _sender: &NSObject) {
            self.install(Model::Speech);
        }

        #[unsafe(method(installCorrectionModel:))]
        fn install_correction_model(&self, _sender: &NSObject) {
            self.install(Model::Correction);
        }

        #[unsafe(method(copyLastTranscript:))]
        fn copy_last_transcript(&self, _sender: &NSObject) {
            let text = self.ivars().window.transcript();
            let outcome = copy_transcript(Some(&text), write_clipboard);
            if matches!(*self.ivars().state.borrow(), State::Idle) {
                self.set_state(State::Idle, copy_status(outcome));
            }
        }

        #[unsafe(method(saveSettings:))]
        fn save_settings(&self, _sender: &NSObject) {
            if !matches!(*self.ivars().state.borrow(), State::Idle) {
                return;
            }
            let Some(preferences) = self.ivars().window.preferences() else {
                return;
            };
            let enabling_right_option = preferences.shortcuts.contains(Shortcut::RightOptionSpace)
                && !self.ivars().preferences.get().shortcuts.contains(Shortcut::RightOptionSpace);
            match self.configure_shortcuts(preferences, true) {
                Ok(()) => {
                    self.ivars().preferences.set(preferences);
                    if self.ivars().shortcuts.borrow().right_option_is_focused() {
                        self.set_state(State::Idle, RIGHT_OPTION_FOCUSED);
                        if enabling_right_option {
                            right_option::request_accessibility();
                        }
                    } else {
                        self.set_state(State::Idle, "Settings saved.");
                    }
                }
                Err(error) => self.set_state(State::Idle, &error.to_string()),
            }
        }

        #[unsafe(method(requestShortcutAccessibility:))]
        fn request_shortcut_accessibility(&self, _sender: &NSObject) {
            if matches!(*self.ivars().state.borrow(), State::Idle)
                && self.ivars().shortcuts.borrow().right_option_is_focused()
            {
                right_option::request_accessibility();
                self.activate_global_right_option();
            }
        }

        #[unsafe(method(quitApp:))]
        fn quit(&self, _sender: &NSMenuItem) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        #[unsafe(method(pollUpdates:))]
        fn poll_updates(&self, _timer: &NSTimer) {
            let update = match &*self.ivars().state.borrow() {
                State::RequestingPermission(_, receiver) => receiver.try_recv(),
                State::Working(receiver) => receiver.try_recv().map(|first| receiver.try_iter().last().unwrap_or(first)),
                State::Transcribing(receiver) => receiver.try_recv().map(Update::Transcript),
                _ => Err(mpsc::TryRecvError::Empty),
            };
            match update {
                Ok(update) => self.apply_update(update),
                Err(mpsc::TryRecvError::Disconnected) => self.set_state(State::Idle, "The task stopped unexpectedly"),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let recording_finished = matches!(
                &*self.ivars().state.borrow(),
                State::Recording(_, recording) if !recording.is_recording()
            );
            if recording_finished {
                self.finish_recording();
            }
        }
    }
);

impl Controller {
    fn new(main: MainThreadMarker) -> Retained<Self> {
        let saved = Storage::resolve().and_then(|storage| Ok(Preferences::load(&storage.config)?));
        let (preferences, status) = match saved {
            Ok(preferences) => (preferences, READY.to_owned()),
            Err(error) => (Preferences::default(), error.to_string()),
        };
        let this = Self::alloc(main).set_ivars(ControllerIvars {
            menu: Menu::new(main),
            window: MainWindow::new(main, preferences),
            dictionary: DictionaryEditor::new(main),
            completion_sound: NSSound::soundNamed(&NSString::from_str("Tink")),
            preferences: Cell::new(preferences),
            shortcuts: RefCell::new(ShortcutListeners::default()),
            state: RefCell::new(State::Idle),
            launch_state: Cell::new(LaunchState::Starting),
            timer: RefCell::new(None),
        });
        // SAFETY: The Rust ivars are initialized before calling NSObject's init.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let menu = &this.ivars().menu;
        // SAFETY: Each action selector was registered above and the controller lives
        // for the duration of NSApplication's event loop.
        unsafe {
            for item in [
                &menu.open,
                &menu.dictionary,
                &menu.record,
                &menu.copy_last,
                &menu.install,
                &menu.quit,
                &menu.app_quit,
            ] {
                item.setTarget(Some(this.as_super()));
            }
        }
        this.ivars().window.set_target(this.as_super());
        this.ivars()
            .window
            .window
            .setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this.set_state(State::Idle, &status);
        this
    }

    fn configure_shortcuts(&self, preferences: Preferences, save: bool) -> super::Result<()> {
        let mut installed = self.ivars().shortcuts.borrow_mut();
        let selected = preferences.shortcuts;
        let mut prepared = Vec::new();
        let mut startup_error = None;
        for shortcut in selected.iter().filter(|shortcut| shortcut.uses_carbon()) {
            if installed
                .carbon
                .iter()
                .any(|hotkey| hotkey.current() == shortcut)
            {
                continue;
            }
            let registration = Registration::new(self.mtm(), shortcut).and_then(|registration| {
                let mut hotkey = Hotkey::new(self.mtm(), self.shortcut_handler(shortcut))?;
                hotkey.apply(registration);
                Ok(Some(hotkey))
            });
            prepared.extend(prepare_registration(
                registration,
                save,
                &mut startup_error,
            )?);
        }

        let enable_pair = selected.contains(Shortcut::RightCommandOption);
        let pair = (enable_pair && installed.right_command_option.is_none())
            .then(|| {
                ModifierShortcut::new(
                    self.mtm(),
                    &self.ivars().window.window,
                    self.shortcut_handler(Shortcut::RightCommandOption),
                )
            })
            .transpose();
        let new_pair = prepare_registration(pair, save, &mut startup_error)?;
        let enable_right_option = selected.contains(Shortcut::RightOptionSpace);
        let right_option = (enable_right_option
            && (installed.right_option_space.is_none()
                || (installed.right_option_is_focused() && right_option::has_accessibility())))
        .then(|| self.make_right_option())
        .transpose();
        let new_right_option = prepare_registration(right_option, save, &mut startup_error)?;

        // Keep the running shortcuts until the entire requested set is prepared and saved.
        if save {
            preferences.save(&Storage::resolve()?.config)?;
        }
        installed
            .carbon
            .retain(|hotkey| selected.contains(hotkey.current()));
        installed.carbon.extend(prepared);
        if !enable_pair || new_pair.is_some() {
            installed.right_command_option = new_pair;
        }
        if !enable_right_option || new_right_option.is_some() {
            installed.right_option_space = new_right_option;
        }
        startup_error.map_or(Ok(()), |error| Err(error.into()))
    }

    fn shortcut_handler(&self, shortcut: Shortcut) -> impl Fn(Edge) + 'static {
        let weak = Weak::new(self);
        move |edge| {
            if let Some(controller) = weak.load() {
                controller.handle_shortcut(shortcut, edge);
            }
        }
    }

    fn make_right_option(&self) -> io::Result<RightOptionShortcut> {
        RightOptionShortcut::new(
            self.mtm(),
            &self.ivars().window.window,
            self.shortcut_handler(Shortcut::RightOptionSpace),
        )
    }

    fn activate_global_right_option(&self) {
        if !matches!(*self.ivars().state.borrow(), State::Idle)
            || !self.ivars().shortcuts.borrow().right_option_is_focused()
            || !right_option::has_accessibility()
        {
            return;
        }
        if let Ok(shortcut) = self.make_right_option()
            && shortcut.is_global()
        {
            self.ivars().shortcuts.borrow_mut().right_option_space = Some(shortcut);
            self.set_state(State::Idle, "Right Option + Space is ready in other apps.");
        }
    }

    fn cancel_focused_shortcut(&self) {
        let shortcuts = self.ivars().shortcuts.borrow();
        if let Some(pair) = &shortcuts.right_command_option {
            pair.reset();
        }
        if let Some(option_space) = &shortcuts.right_option_space {
            option_space.reset_focused();
        }
        let right_option_is_focused = shortcuts.right_option_is_focused();
        drop(shortcuts);
        let recording = match &*self.ivars().state.borrow() {
            State::Recording(RecordOrigin::Shortcut(Shortcut::RightCommandOption), _) => true,
            State::Recording(RecordOrigin::Shortcut(Shortcut::RightOptionSpace), _) => {
                right_option_is_focused
            }
            _ => false,
        };
        if recording {
            self.set_state(State::Idle, "Recording canceled because v2t lost focus.");
        }
    }

    fn handle_shortcut(&self, shortcut: Shortcut, edge: Edge) {
        let action = shortcut_action(&self.ivars().state.borrow(), shortcut, edge);
        match action {
            Some(ShortcutAction::Start) => {
                self.request_microphone(RecordOrigin::Shortcut(shortcut))
            }
            Some(ShortcutAction::Stop) => self.finish_recording(),
            None => {}
        }
    }

    #[allow(deprecated)] // Activate the window on macOS 12 and later.
    fn present_window(&self) {
        let window = &self.ivars().window.window;
        if window.isMiniaturized() {
            window.deminiaturize(None);
        }
        window.makeKeyAndOrderFront(None);
        NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
    }

    fn request_microphone(&self, origin: RecordOrigin) {
        let language = self.ivars().preferences.get().speech().language;
        let ready = Storage::resolve()
            .and_then(|storage| Ok(LocalModels::new(storage.data)?))
            .is_ok_and(|models| models.speech_available(language));
        if !ready {
            eprintln!("v2t: Install the speech model and whisper-server first");
            self.set_state(
                State::Idle,
                "Install the speech model and whisper-server first",
            );
            return;
        }
        // SAFETY: AVMediaTypeAudio is the documented media type for microphone access.
        unsafe {
            let Some(audio) = AVMediaTypeAudio else {
                self.set_state(State::Idle, "Microphone capture is unavailable");
                return;
            };
            match AVCaptureDevice::authorizationStatusForMediaType(audio) {
                AVAuthorizationStatus::Authorized => self.start_recording(origin),
                AVAuthorizationStatus::NotDetermined => {
                    let (send, receive) = mpsc::channel();
                    self.set_state(
                        State::RequestingPermission(origin, receive),
                        match origin {
                            RecordOrigin::Button => "Waiting for microphone access…",
                            RecordOrigin::Shortcut(_) => {
                                "Allow microphone access, then hold the shortcut again."
                            }
                        },
                    );
                    let response = RcBlock::new(move |granted: Bool| {
                        let _ = send.send(Update::Microphone(granted.as_bool()));
                    });
                    AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &response);
                }
                _ => self.set_state(State::Idle, MIC_SETTINGS),
            }
        }
    }

    fn start_from_launcher(&self) {
        self.present_window();
        if matches!(*self.ivars().state.borrow(), State::Idle) {
            self.request_microphone(RecordOrigin::Button);
        }
    }

    fn start_recording(&self, origin: RecordOrigin) {
        let options = self.ivars().preferences.get().speech();
        let result = Storage::resolve().and_then(|storage| {
            let (speech, transcript) = pipeline::start_recording(storage, options)?;
            Ok(Recording::start(speech, transcript)?)
        });
        match result {
            Ok(recording) => self.set_state(State::Recording(origin, recording), "Recording…"),
            Err(error) => self.set_state(State::Idle, &error.to_string()),
        }
    }

    fn finish_recording(&self) {
        let State::Recording(_, recording) = self.ivars().state.replace(State::Idle) else {
            return;
        };
        self.set_state(State::Transcribing(recording.finish()), "Transcribing…");
    }

    fn install(&self, model: Model) {
        if !matches!(*self.ivars().state.borrow(), State::Idle) {
            return;
        }
        let (status, ready) = match model {
            Model::Speech => ("Preparing speech model…", "Speech model ready"),
            Model::Correction => ("Preparing local AI model…", "Typhoon 2.5 4B is ready"),
        };
        let language = self.ivars().preferences.get().speech().language;
        let (send, receive) = mpsc::channel();
        match thread::Builder::new()
            .name("v2t-model-install".into())
            .spawn(move || {
                let result = (|| -> super::Result<()> {
                    let storage = Storage::resolve()?;
                    let models = LocalModels::new(storage.data)?;
                    let report = |progress: &str| {
                        let _ = send.send(Update::ModelProgress(progress.to_owned()));
                    };
                    match model {
                        Model::Speech => models.install_speech(language, report)?,
                        Model::Correction => models.install_correction(report)?,
                    }
                    Ok(())
                })();
                let _ = send.send(Update::Model(
                    result.map_err(|error| error.to_string()),
                    ready,
                ));
            }) {
            Ok(_) => self.set_state(State::Working(receive), status),
            Err(error) => {
                self.set_state(State::Idle, &format!("Could not start the task ({error})"))
            }
        }
    }

    fn apply_update(&self, update: Update) {
        let status = match update {
            Update::Microphone(true) => {
                let origin = match &*self.ivars().state.borrow() {
                    State::RequestingPermission(origin, _) => *origin,
                    _ => return,
                };
                return match origin {
                    RecordOrigin::Button => self.start_recording(origin),
                    RecordOrigin::Shortcut(_) => self.set_state(
                        State::Idle,
                        "Microphone ready. Hold the shortcut to record.",
                    ),
                };
            }
            Update::Microphone(false) => MIC_SETTINGS,
            Update::ModelProgress(progress) => {
                self.set_status(&NSString::from_str(&progress));
                return;
            }
            Update::Model(Ok(()), ready) => ready,
            Update::Transcript(Ok(transcript)) => {
                let status = copy_status(copy_transcript(transcript.text.as_deref(), |text| {
                    self.ivars().window.set_transcript(text);
                    self.ivars().menu.copy_last.setEnabled(true);
                    let copied = write_clipboard(text);
                    self.play_completion_sound();
                    copied
                }));
                if let Some(error) = transcript.partial_failure {
                    return self.set_state(
                        State::Idle,
                        &format!("{status}. Dictation may be incomplete. {error}"),
                    );
                }
                if transcript.correction_fallback {
                    return self.set_state(
                        State::Idle,
                        &format!("{status}. AI could not correct some text. Dictionary applied."),
                    );
                }
                status
            }
            Update::Model(Err(error), _) | Update::Transcript(Err(error)) => {
                return self.set_state(State::Idle, &error);
            }
        };
        self.set_state(State::Idle, status);
    }

    fn play_completion_sound(&self) {
        if self
            .ivars()
            .completion_sound
            .as_ref()
            .is_none_or(|sound| !sound.play())
        {
            NSBeep();
        }
    }

    fn set_status(&self, status: &NSString) {
        let menu = &self.ivars().menu;
        menu.status.setTitle(status);
        self.ivars().window.set_status(status);
        if let Some(button) = menu.bar_item.button(self.mtm()) {
            button.setToolTip(Some(status));
        }
    }

    fn set_state(&self, state: State, status: &str) {
        let idle = matches!(state, State::Idle);
        let recording = matches!(state, State::Recording(_, _));
        let working = matches!(state, State::Transcribing(_) | State::Working(_));
        let menu = &self.ivars().menu;
        self.set_status(&NSString::from_str(status));
        let record_title = NSString::from_str(if recording {
            "Stop and Copy"
        } else {
            "Start Recording"
        });
        menu.record.setTitle(&record_title);
        menu.record.setEnabled(idle || recording);
        menu.install.setEnabled(idle);
        menu.quit.setEnabled(!working);
        menu.app_quit.setEnabled(!working);
        self.ivars().window.update(
            &record_title,
            idle,
            recording,
            self.ivars()
                .shortcuts
                .borrow()
                .right_option_space
                .as_ref()
                .map(RightOptionShortcut::is_global),
        );
        if let Some(button) = menu.bar_item.button(self.mtm()) {
            let title = match &state {
                State::Recording(_, _) => "v2t ●",
                State::Transcribing(_) | State::Working(_) => "v2t …",
                _ => "v2t",
            };
            button.setTitle(&NSString::from_str(title));
        }
        self.ivars().state.replace(state);

        if idle {
            self.stop_polling();
        } else if self.ivars().timer.borrow().is_none() {
            // SAFETY: This selector accepts a timer argument on the main run loop.
            // Common modes keep status updates visible while the menu is open.
            let timer = unsafe {
                let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                    0.2,
                    self.as_super(),
                    sel!(pollUpdates:),
                    None,
                    true,
                );
                NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
                timer
            };
            self.ivars().timer.replace(Some(timer));
        }
    }

    fn stop_polling(&self) {
        if let Some(timer) = self.ivars().timer.take() {
            timer.invalidate();
        }
    }
}

impl Menu {
    fn new(main: MainThreadMarker) -> Self {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(main), &NSString::from_str("v2t"));
        menu.setAutoenablesItems(false);

        let status = menu_item(main, READY, None);
        status.setEnabled(false);
        let open = menu_item(main, "Settings…", Some(sel!(showWindow:)));
        let dictionary = menu_item(main, "Dictionary…", Some(sel!(showDictionary:)));
        let record = menu_item(main, "Start Recording", Some(sel!(toggleRecording:)));
        let copy_last = menu_item(
            main,
            "Copy Last Transcript",
            Some(sel!(copyLastTranscript:)),
        );
        copy_last.setEnabled(false);
        let install = menu_item(main, "Install Speech Model", Some(sel!(installModel:)));
        let quit = menu_item(main, "Quit v2t", Some(sel!(quitApp:)));
        for item in [
            &status,
            &open,
            &dictionary,
            &record,
            &copy_last,
            &install,
            &quit,
        ] {
            menu.addItem(item);
        }

        let main_menu = NSMenu::new(main);
        let app_quit = menu_item(main, "Quit v2t", Some(sel!(quitApp:)));
        app_quit.setKeyEquivalent(&NSString::from_str("q"));
        let app_menu = NSMenu::initWithTitle(NSMenu::alloc(main), &NSString::from_str("v2t"));
        app_menu.setAutoenablesItems(false);
        app_menu.addItem(&app_quit);
        add_submenu(&main_menu, main, &app_menu);

        let cut = menu_item(main, "Cut", Some(sel!(cut:)));
        cut.setKeyEquivalent(&NSString::from_str("x"));
        let copy = menu_item(main, "Copy", Some(sel!(copy:)));
        copy.setKeyEquivalent(&NSString::from_str("c"));
        let paste = menu_item(main, "Paste", Some(sel!(paste:)));
        paste.setKeyEquivalent(&NSString::from_str("v"));
        let select_all = menu_item(main, "Select All", Some(sel!(selectAll:)));
        select_all.setKeyEquivalent(&NSString::from_str("a"));
        let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(main), &NSString::from_str("Edit"));
        for item in [&cut, &copy, &paste, &select_all] {
            edit_menu.addItem(item);
        }
        add_submenu(&main_menu, main, &edit_menu);

        let close = menu_item(main, "Close Window", Some(sel!(performClose:)));
        close.setKeyEquivalent(&NSString::from_str("w"));
        let window_menu = NSMenu::initWithTitle(NSMenu::alloc(main), &NSString::from_str("Window"));
        window_menu.addItem(&close);
        add_submenu(&main_menu, main, &window_menu);

        let bar_item =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        bar_item.setMenu(Some(&menu));
        Self {
            bar_item,
            status,
            open,
            dictionary,
            record,
            copy_last,
            install,
            quit,
            main_menu,
            app_quit,
        }
    }
}

fn add_submenu(menu: &NSMenu, main: MainThreadMarker, submenu: &NSMenu) {
    let item = NSMenuItem::new(main);
    item.setTitle(&submenu.title());
    menu.addItem(&item);
    menu.setSubmenu_forItem(Some(submenu), &item);
}

fn menu_item(main: MainThreadMarker, title: &str, action: Option<Sel>) -> Retained<NSMenuItem> {
    // SAFETY: The caller provides a registered controller action or no action.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(main),
            &NSString::from_str(title),
            action,
            &NSString::from_str(""),
        )
    }
}

fn copy_status(outcome: CopyOutcome) -> &'static str {
    match outcome {
        CopyOutcome::Copied => "Copied to clipboard. Press ⌘V to paste",
        CopyOutcome::NoSpeech => "No speech detected",
        CopyOutcome::Failed => "Could not copy the transcript",
    }
}

fn write_clipboard(text: &str) -> bool {
    let pasteboard = NSPasteboard::generalPasteboard();
    let text = NSString::from_str(text);
    pasteboard.clearContents();
    // SAFETY: This framework constant is the standard plain text pasteboard type.
    pasteboard.setString_forType(&text, unsafe { NSPasteboardTypeString })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_edges_leave_the_pending_system_permission_prompt_in_place() {
        let (_send, receive) = mpsc::channel();
        let shortcut = Shortcut::CommandShiftSpace;
        let state = State::RequestingPermission(RecordOrigin::Shortcut(shortcut), receive);
        assert_eq!(shortcut_action(&state, shortcut, Edge::Pressed), None);
        assert_eq!(
            shortcut_action(&state, Shortcut::RightOptionSpace, Edge::Released),
            None
        );
    }

    #[test]
    fn a_shortcut_only_starts_when_ready_and_never_cancels_a_manual_request() {
        let (_send, receive) = mpsc::channel();
        let shortcut = Shortcut::RightOptionSpace;
        let state = State::RequestingPermission(RecordOrigin::Button, receive);
        assert_eq!(shortcut_action(&state, shortcut, Edge::Pressed), None);
        assert_eq!(shortcut_action(&state, shortcut, Edge::Released), None);
        assert_eq!(
            shortcut_action(&State::Idle, shortcut, Edge::Released),
            None
        );
        assert_eq!(
            shortcut_action(&State::Idle, shortcut, Edge::Pressed),
            Some(ShortcutAction::Start)
        );
    }
}
