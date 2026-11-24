mod recording;

use std::{cell::RefCell, process::ExitCode, sync::mpsc, thread};

use block2::RcBlock;
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{Bool, Sel},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSMenu, NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_foundation::{NSBundle, NSObject, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer};

use super::{
    SpeechOptions, Storage,
    clipboard::{CopyOutcome, copy_transcript},
    pipeline,
};
use crate::models::LocalModels;
use recording::Recording;

const READY: &str = "Ready to record";
const MIC_SETTINGS: &str = "Allow v2t under Microphone in your Mac’s privacy settings";

pub(super) fn is_app_bundle() -> bool {
    autoreleasepool(|_| {
        NSBundle::mainBundle()
            .bundleIdentifier()
            .is_some_and(|id| id.to_string() == "com.lawdalana.v2t")
    })
}

pub(super) fn run() -> ExitCode {
    let Some(main) = MainThreadMarker::new() else {
        eprintln!("v2t: The menu bar app must run on the main thread.");
        return ExitCode::FAILURE;
    };
    autoreleasepool(|_| {
        let app = NSApplication::sharedApplication(main);
        let controller = Controller::new(main);
        app.run();
        controller.stop_polling();
    });
    ExitCode::SUCCESS
}

enum State {
    Idle,
    RequestingPermission(mpsc::Receiver<Update>),
    Recording(Recording),
    Working(mpsc::Receiver<Update>),
}

enum Update {
    Microphone(bool),
    Transcript(Result<Option<String>, String>),
    Model(Result<(), String>),
}

struct Menu {
    bar_item: Retained<NSStatusItem>,
    status: Retained<NSMenuItem>,
    record: Retained<NSMenuItem>,
    install: Retained<NSMenuItem>,
    quit: Retained<NSMenuItem>,
}

struct ControllerIvars {
    menu: Menu,
    state: RefCell<State>,
    timer: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    // SAFETY: NSObject has no additional subclassing requirements. AppKit invokes
    // the menu actions and the timer on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    struct Controller;

    impl Controller {
        #[unsafe(method(toggleRecording:))]
        fn toggle_recording(&self, _sender: &NSMenuItem) {
            let state = self.ivars().state.borrow();
            match &*state {
                State::Idle => {
                    drop(state);
                    self.request_microphone();
                }
                State::Recording(_) => {
                    drop(state);
                    self.finish_recording();
                }
                _ => {}
            }
        }

        #[unsafe(method(installModel:))]
        fn install_model(&self, _sender: &NSMenuItem) {
            if matches!(*self.ivars().state.borrow(), State::Idle) {
                self.start_task("Downloading speech model…", || {
                    let result = (|| -> super::Result<()> {
                        let storage = Storage::resolve()?;
                        let models = LocalModels::new(storage.data)?;
                        models.install_speech(SpeechOptions::default().language, |_| {})?;
                        Ok(())
                    })();
                    Update::Model(result.map_err(|error| error.to_string()))
                });
            }
        }

        #[unsafe(method(quitApp:))]
        fn quit(&self, _sender: &NSMenuItem) {
            if !matches!(*self.ivars().state.borrow(), State::Working(_)) {
                self.set_state(State::Idle, READY);
                NSApplication::sharedApplication(self.mtm()).terminate(None);
            }
        }

        #[unsafe(method(pollUpdates:))]
        fn poll_updates(&self, _timer: &NSTimer) {
            let update = match &*self.ivars().state.borrow() {
                State::RequestingPermission(receiver) | State::Working(receiver) => receiver.try_recv(),
                _ => Err(mpsc::TryRecvError::Empty),
            };
            match update {
                Ok(update) => self.apply_update(update),
                Err(mpsc::TryRecvError::Disconnected) => self.set_state(State::Idle, "The task stopped unexpectedly"),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let recording_finished = matches!(
                &*self.ivars().state.borrow(),
                State::Recording(recording) if !recording.is_recording()
            );
            if recording_finished {
                self.finish_recording();
            }
        }
    }
);

impl Controller {
    fn new(main: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(main).set_ivars(ControllerIvars {
            menu: Menu::new(main),
            state: RefCell::new(State::Idle),
            timer: RefCell::new(None),
        });
        // SAFETY: The Rust ivars are initialized before calling NSObject's init.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let menu = &this.ivars().menu;
        // SAFETY: Each action selector was registered above and the controller lives
        // for the duration of NSApplication's event loop.
        unsafe {
            for item in [&menu.record, &menu.install, &menu.quit] {
                item.setTarget(Some(this.as_super()));
            }
        }
        this.set_state(State::Idle, READY);
        this
    }

    fn request_microphone(&self) {
        let ready = Storage::resolve()
            .and_then(|storage| Ok(LocalModels::new(storage.data)?))
            .is_ok_and(|models| models.speech_available(SpeechOptions::default().language));
        if !ready {
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
                AVAuthorizationStatus::Authorized => self.start_recording(),
                AVAuthorizationStatus::NotDetermined => {
                    let (send, receive) = mpsc::channel();
                    self.set_state(
                        State::RequestingPermission(receive),
                        "Waiting for microphone access…",
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

    fn start_recording(&self) {
        match Recording::start() {
            Ok(recording) => self.set_state(State::Recording(recording), "Recording…"),
            Err(error) => self.set_state(State::Idle, &error.to_string()),
        }
    }

    fn finish_recording(&self) {
        let State::Recording(recording) = self.ivars().state.replace(State::Idle) else {
            return;
        };
        match recording.finish() {
            Ok(wave) => self.start_task("Transcribing…", move || {
                let options = SpeechOptions::default();
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
            Update::Microphone(true) => return self.start_recording(),
            Update::Microphone(false) => MIC_SETTINGS,
            Update::Model(Ok(())) => "Speech model ready",
            Update::Transcript(Ok(text)) => match copy_transcript(text.as_deref(), write_clipboard)
            {
                CopyOutcome::Copied => "Copied to clipboard. Press ⌘V to paste",
                CopyOutcome::NoSpeech => "No speech detected",
                CopyOutcome::Failed => "Could not copy the transcript",
            },
            Update::Model(Err(error)) | Update::Transcript(Err(error)) => {
                return self.set_state(State::Idle, &error);
            }
        };
        self.set_state(State::Idle, status);
    }

    fn set_state(&self, state: State, status: &str) {
        let idle = matches!(state, State::Idle);
        let recording = matches!(state, State::Recording(_));
        let working = matches!(state, State::Working(_));
        let menu = &self.ivars().menu;
        menu.status.setTitle(&NSString::from_str(status));
        menu.record.setTitle(&NSString::from_str(if recording {
            "Stop and Copy"
        } else {
            "Start Recording"
        }));
        menu.record.setEnabled(idle || recording);
        menu.install.setEnabled(idle);
        menu.quit.setEnabled(!working);
        if let Some(button) = menu.bar_item.button(self.mtm()) {
            let title = match &state {
                State::Recording(_) => "v2t ●",
                State::Working(_) => "v2t …",
                _ => "v2t",
            };
            button.setTitle(&NSString::from_str(title));
            button.setToolTip(Some(&NSString::from_str(status)));
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
        let record = menu_item(main, "Start Recording", Some(sel!(toggleRecording:)));
        let install = menu_item(main, "Install Speech Model", Some(sel!(installModel:)));
        let quit = menu_item(main, "Quit v2t", Some(sel!(quitApp:)));
        for item in [&status, &record, &install, &quit] {
            menu.addItem(item);
        }

        let bar_item =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        bar_item.setMenu(Some(&menu));
        Self {
            bar_item,
            status,
            record,
            install,
            quit,
        }
    }
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

fn write_clipboard(text: &str) -> bool {
    let pasteboard = NSPasteboard::generalPasteboard();
    let text = NSString::from_str(text);
    pasteboard.clearContents();
    // SAFETY: This framework constant is the standard plain text pasteboard type.
    pasteboard.setString_forType(&text, unsafe { NSPasteboardTypeString })
}
