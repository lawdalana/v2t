mod hotkey;
mod launcher;
mod modifiers;
mod preferences;
mod recording;
mod window;

use std::{
    cell::{Cell, RefCell},
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
    SpeechOptions, Storage,
    clipboard::{CopyOutcome, copy_transcript},
    pipeline,
};
use crate::models::LocalModels;
use hotkey::{Edge, Hotkey, Registration};
use modifiers::ModifierShortcut;
use preferences::{Preferences, Shortcut};
use recording::Recording;
use window::MainWindow;

const READY: &str = "Ready to record";
const MIC_SETTINGS: &str = "Allow v2t under Microphone in your Mac’s privacy settings";

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
    Working(mpsc::Receiver<Update>),
}

#[derive(Clone, Copy)]
enum RecordOrigin {
    Button,
    Shortcut,
}

#[derive(Debug, PartialEq, Eq)]
enum ShortcutAction {
    Start,
    Stop,
}

fn shortcut_action(state: &State, edge: Edge) -> Option<ShortcutAction> {
    match (state, edge) {
        (State::Idle, Edge::Pressed) => Some(ShortcutAction::Start),
        (State::Recording(RecordOrigin::Shortcut, _), Edge::Released) => Some(ShortcutAction::Stop),
        _ => None,
    }
}

enum Update {
    Microphone(bool),
    Transcript(Result<Option<String>, String>),
    Model(Result<(), String>),
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
    completion_sound: Option<Retained<NSSound>>,
    preferences: Cell<Preferences>,
    hotkey: RefCell<Option<Hotkey>>,
    modifier_shortcut: RefCell<Option<ModifierShortcut>>,
    state: RefCell<State>,
    launch_state: Cell<LaunchState>,
    timer: RefCell<Option<Retained<NSTimer>>>,
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
            if let Err(error) = self.configure_shortcut(self.ivars().preferences.get(), false) {
                eprintln!("v2t: {error}");
                self.set_state(State::Idle, &error.to_string());
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
            if matches!(*self.ivars().state.borrow(), State::Working(_)) {
                return NSApplicationTerminateReply::TerminateCancel;
            }
            self.set_state(State::Idle, READY);
            NSApplicationTerminateReply::TerminateNow
        }

        #[unsafe(method(applicationDidResignActive:))]
        fn applicationDidResignActive(&self, _notification: &NSNotification) {
            self.cancel_focused_shortcut();
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
            if matches!(*self.ivars().state.borrow(), State::Idle) {
                let language = self.ivars().preferences.get().speech().language;
                self.start_task("Downloading speech model…", move || {
                    let result = (|| -> super::Result<()> {
                        let storage = Storage::resolve()?;
                        let models = LocalModels::new(storage.data)?;
                        models.install_speech(language, |_| {})?;
                        Ok(())
                    })();
                    Update::Model(result.map_err(|error| error.to_string()))
                });
            }
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
            match self.configure_shortcut(preferences, true) {
                Ok(()) => {
                    self.ivars().preferences.set(preferences);
                    self.set_state(State::Idle, "Settings saved.");
                }
                Err(error) => self.set_state(State::Idle, &error.to_string()),
            }
        }

        #[unsafe(method(quitApp:))]
        fn quit(&self, _sender: &NSMenuItem) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        #[unsafe(method(pollUpdates:))]
        fn poll_updates(&self, _timer: &NSTimer) {
            let update = match &*self.ivars().state.borrow() {
                State::RequestingPermission(_, receiver) | State::Working(receiver) => receiver.try_recv(),
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
            completion_sound: NSSound::soundNamed(&NSString::from_str("Tink")),
            preferences: Cell::new(preferences),
            hotkey: RefCell::new(None),
            modifier_shortcut: RefCell::new(None),
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

    fn configure_shortcut(&self, preferences: Preferences, save: bool) -> super::Result<()> {
        if preferences.shortcut == Shortcut::RightCommandOption {
            return self.configure_modifier_shortcut(preferences, save);
        }
        let mut slot = self.ivars().hotkey.borrow_mut();
        if slot.is_none() && preferences.shortcut != Shortcut::Off {
            let weak = Weak::new(self);
            *slot = Some(Hotkey::new(self.mtm(), move |edge| {
                if let Some(controller) = weak.load() {
                    controller.handle_shortcut(edge);
                }
            })?);
        }
        let prepared = slot
            .as_ref()
            .filter(|hotkey| hotkey.current() != preferences.shortcut)
            .map(|_| Registration::new(self.mtm(), preferences.shortcut))
            .transpose()?;
        if save {
            preferences.save(&Storage::resolve()?.config)?;
        }
        if let (Some(hotkey), Some(registration)) = (slot.as_mut(), prepared) {
            hotkey.apply(registration);
        }
        self.ivars().modifier_shortcut.borrow_mut().take();
        Ok(())
    }

    fn configure_modifier_shortcut(
        &self,
        preferences: Preferences,
        save: bool,
    ) -> super::Result<()> {
        let mut slot = self.ivars().modifier_shortcut.borrow_mut();
        let replacement = if slot.is_some() {
            None
        } else {
            let weak = Weak::new(self);
            Some(ModifierShortcut::new(
                self.mtm(),
                &self.ivars().window.window,
                move |edge| {
                    if let Some(controller) = weak.load() {
                        controller.handle_shortcut(edge);
                    }
                },
            )?)
        };
        if save {
            preferences.save(&Storage::resolve()?.config)?;
        }
        self.ivars().hotkey.borrow_mut().take();
        if let Some(replacement) = replacement {
            *slot = Some(replacement);
        }
        Ok(())
    }

    fn cancel_focused_shortcut(&self) {
        if let Some(shortcut) = self.ivars().modifier_shortcut.borrow().as_ref() {
            shortcut.reset();
        } else {
            return;
        }
        let recording = matches!(
            *self.ivars().state.borrow(),
            State::Recording(RecordOrigin::Shortcut, _)
        );
        if recording {
            self.set_state(State::Idle, "Recording canceled because v2t lost focus.");
        }
    }

    fn handle_shortcut(&self, edge: Edge) {
        let action = shortcut_action(&self.ivars().state.borrow(), edge);
        match action {
            Some(ShortcutAction::Start) => self.request_microphone(RecordOrigin::Shortcut),
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
                            RecordOrigin::Shortcut => {
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
        match Recording::start() {
            Ok(recording) => self.set_state(State::Recording(origin, recording), "Recording…"),
            Err(error) => self.set_state(State::Idle, &error.to_string()),
        }
    }

    fn finish_recording(&self) {
        let State::Recording(_, recording) = self.ivars().state.replace(State::Idle) else {
            return;
        };
        let options = self.ivars().preferences.get().speech();
        match recording.finish() {
            Ok(wave) => self.start_task("Transcribing…", move || {
                let result = Storage::resolve()
                    .and_then(|storage| pipeline::transcribe(&storage, &options, &wave));
                Update::Transcript(result.map_err(|error| error.to_string()))
            }),
            Err(error) => self.set_state(State::Idle, &error.to_string()),
        }
    }

    fn start_task(&self, status: &str, work: impl FnOnce() -> Update + Send + 'static) {
        let (send, receive) = mpsc::channel();
        match thread::Builder::new()
            .name("v2t-dictation".into())
            .spawn(move || {
                let _ = send.send(work());
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
                    RecordOrigin::Shortcut => self.set_state(
                        State::Idle,
                        "Microphone ready. Hold the shortcut to record.",
                    ),
                };
            }
            Update::Microphone(false) => MIC_SETTINGS,
            Update::Model(Ok(())) => "Speech model ready",
            Update::Transcript(Ok(text)) => copy_status(copy_transcript(text.as_deref(), |text| {
                self.ivars().window.set_transcript(text);
                self.ivars().menu.copy_last.setEnabled(true);
                let copied = write_clipboard(text);
                self.play_completion_sound();
                copied
            })),
            Update::Model(Err(error)) | Update::Transcript(Err(error)) => {
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

    fn set_state(&self, state: State, status: &str) {
        let idle = matches!(state, State::Idle);
        let recording = matches!(state, State::Recording(_, _));
        let working = matches!(state, State::Working(_));
        let menu = &self.ivars().menu;
        let status = NSString::from_str(status);
        let record_title = NSString::from_str(if recording {
            "Stop and Copy"
        } else {
            "Start Recording"
        });
        menu.status.setTitle(&status);
        menu.record.setTitle(&record_title);
        menu.record.setEnabled(idle || recording);
        menu.install.setEnabled(idle);
        menu.quit.setEnabled(!working);
        menu.app_quit.setEnabled(!working);
        self.ivars()
            .window
            .update(&record_title, &status, idle, recording);
        if let Some(button) = menu.bar_item.button(self.mtm()) {
            let title = match &state {
                State::Recording(_, _) => "v2t ●",
                State::Working(_) => "v2t …",
                _ => "v2t",
            };
            button.setTitle(&NSString::from_str(title));
            button.setToolTip(Some(&status));
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
        let record = menu_item(main, "Start Recording", Some(sel!(toggleRecording:)));
        let copy_last = menu_item(
            main,
            "Copy Last Transcript",
            Some(sel!(copyLastTranscript:)),
        );
        copy_last.setEnabled(false);
        let install = menu_item(main, "Install Speech Model", Some(sel!(installModel:)));
        let quit = menu_item(main, "Quit v2t", Some(sel!(quitApp:)));
        for item in [&status, &open, &record, &copy_last, &install, &quit] {
            menu.addItem(item);
        }

        let main_menu = NSMenu::new(main);
        let app_quit = menu_item(main, "Quit v2t", Some(sel!(quitApp:)));
        app_quit.setKeyEquivalent(&NSString::from_str("q"));
        let app_menu = NSMenu::initWithTitle(NSMenu::alloc(main), &NSString::from_str("v2t"));
        app_menu.setAutoenablesItems(false);
        app_menu.addItem(&app_quit);
        add_submenu(&main_menu, main, &app_menu);

        let copy = menu_item(main, "Copy", Some(sel!(copy:)));
        copy.setKeyEquivalent(&NSString::from_str("c"));
        let select_all = menu_item(main, "Select All", Some(sel!(selectAll:)));
        select_all.setKeyEquivalent(&NSString::from_str("a"));
        let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(main), &NSString::from_str("Edit"));
        edit_menu.addItem(&copy);
        edit_menu.addItem(&select_all);
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
        let state = State::RequestingPermission(RecordOrigin::Shortcut, receive);
        assert_eq!(shortcut_action(&state, Edge::Pressed), None);
        assert_eq!(shortcut_action(&state, Edge::Released), None);
    }

    #[test]
    fn a_shortcut_only_starts_when_ready_and_never_cancels_a_manual_request() {
        let (_send, receive) = mpsc::channel();
        let state = State::RequestingPermission(RecordOrigin::Button, receive);
        assert_eq!(shortcut_action(&state, Edge::Pressed), None);
        assert_eq!(shortcut_action(&state, Edge::Released), None);
        assert_eq!(shortcut_action(&State::Idle, Edge::Released), None);
        assert_eq!(
            shortcut_action(&State::Idle, Edge::Pressed),
            Some(ShortcutAction::Start)
        );
    }
}
