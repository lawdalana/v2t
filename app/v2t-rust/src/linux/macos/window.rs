use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained, runtime::Sel, sel};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle, NSBorderType, NSButton, NSColor,
    NSControlSize, NSFont, NSPopUpButton, NSScrollView, NSTextField, NSTextView, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};

use super::{
    READY,
    preferences::{Language, Preferences, Shortcut},
};

const WIDTH: f64 = 508.0;
const HEIGHT: f64 = 460.0;
const MARGIN: f64 = 24.0;
const CONTENT_WIDTH: f64 = WIDTH - MARGIN * 2.0;
const FIELD_X: f64 = 181.0;
const FIELD_WIDTH: f64 = WIDTH - MARGIN - FIELD_X;

pub(super) struct MainWindow {
    pub(super) window: Retained<NSWindow>,
    status: Retained<NSTextField>,
    language: Retained<NSPopUpButton>,
    shortcut: Retained<NSPopUpButton>,
    dictionary: Retained<NSButton>,
    record: Retained<NSButton>,
    install: Retained<NSButton>,
    save: Retained<NSButton>,
    copy: Retained<NSButton>,
    transcript: Retained<NSTextView>,
    placeholder: Retained<NSTextField>,
}

impl MainWindow {
    pub(super) fn new(main: MainThreadMarker, preferences: Preferences) -> Self {
        let bounds = rect(0.0, 0.0, WIDTH, HEIGHT);
        // SAFETY: This window has a valid content rectangle and runs on the main thread.
        // The controller retains it for the app's lifetime, including while it is closed.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(main),
                bounds,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str("v2t"));
        window.center();

        let view = NSView::initWithFrame(NSView::alloc(main), bounds);
        let title = label(
            main,
            "Dictation settings",
            rect(MARGIN, 399.0, CONTENT_WIDTH - 126.0, 38.0),
        );
        title.setFont(Some(&NSFont::boldSystemFontOfSize(29.0)));
        let dictionary = button(
            main,
            "Dictionary…",
            sel!(showDictionary:),
            rect(WIDTH - MARGIN - 114.0, 401.0, 114.0, 34.0),
        );
        let language_label = label(main, "Dictation language", rect(MARGIN, 362.0, 150.0, 20.0));
        let language = popup(
            main,
            Language::OPTIONS.map(Language::label),
            Language::OPTIONS
                .iter()
                .position(|&item| item == preferences.language)
                .unwrap_or(0),
            rect(FIELD_X, 357.0, FIELD_WIDTH, 30.0),
        );
        let shortcut_label = label(main, "Hold to dictate", rect(MARGIN, 320.0, 150.0, 20.0));
        let shortcut = popup(
            main,
            Shortcut::OPTIONS.map(Shortcut::label),
            Shortcut::OPTIONS
                .iter()
                .position(|&item| item == preferences.shortcut)
                .unwrap_or(0),
            rect(FIELD_X, 315.0, FIELD_WIDTH, 30.0),
        );
        let instructions = label(
            main,
            "Hold the shortcut to record. Release it to copy.",
            rect(FIELD_X, 288.0, FIELD_WIDTH, 20.0),
        );
        instructions.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let record = button(
            main,
            "Start Recording",
            sel!(toggleRecording:),
            rect(207.0, 230.0, 136.0, 38.0),
        );
        let install = button(
            main,
            "Install Speech Model",
            sel!(installModel:),
            rect(MARGIN, 230.0, 170.0, 38.0),
        );
        let save = button(
            main,
            "Save Settings",
            sel!(saveSettings:),
            rect(356.0, 230.0, 128.0, 38.0),
        );
        save.setKeyEquivalent(&NSString::from_str("\r"));

        let status = NSTextField::wrappingLabelWithString(&NSString::from_str(READY), main);
        status.setFrame(rect(MARGIN, 181.0, CONTENT_WIDTH, 42.0));
        let transcript_label = label(main, "Transcript", rect(MARGIN, 161.0, CONTENT_WIDTH, 20.0));
        transcript_label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        let copy = button(
            main,
            "Copy",
            sel!(copyLastTranscript:),
            rect(WIDTH - MARGIN - 76.0, 157.0, 76.0, 25.0),
        );
        copy.setControlSize(NSControlSize::Small);
        copy.setEnabled(false);

        let scroll_bounds = rect(MARGIN, MARGIN, CONTENT_WIDTH, 131.0);
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(main), scroll_bounds);
        scroll.setBorderType(NSBorderType::BezelBorder);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);

        let viewport = scroll.contentSize();
        let transcript = NSTextView::initWithFrame(
            NSTextView::alloc(main),
            NSRect::new(NSPoint::new(0.0, 0.0), viewport),
        );
        transcript.setEditable(false);
        transcript.setSelectable(true);
        transcript.setRichText(false);
        transcript.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        transcript.setTextContainerInset(NSSize::new(8.0, 8.0));
        transcript.setMinSize(viewport);
        transcript.setMaxSize(NSSize::new(viewport.width, f64::MAX));
        transcript.setVerticallyResizable(true);
        transcript.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        // SAFETY: The view owns its default text system, which stays attached here.
        if let Some(container) = unsafe { transcript.textContainer() } {
            container.setWidthTracksTextView(true);
        }
        scroll.setDocumentView(Some(&transcript));

        let placeholder = label(
            main,
            "Your transcript will appear here.",
            rect(MARGIN + 13.0, 124.0, CONTENT_WIDTH - 26.0, 22.0),
        );
        placeholder.setTextColor(Some(&NSColor::secondaryLabelColor()));
        for child in [
            &*title as &NSView,
            &*dictionary,
            &*language_label,
            &*language,
            &*shortcut_label,
            &*shortcut,
            &*instructions,
            &*install,
            &*record,
            &*save,
            &*status,
            &*transcript_label,
            &*copy,
            &*scroll,
            &*placeholder,
        ] {
            view.addSubview(child);
        }
        window.setContentView(Some(&view));

        Self {
            window,
            status,
            language,
            shortcut,
            dictionary,
            record,
            install,
            save,
            copy,
            transcript,
            placeholder,
        }
    }

    pub(super) fn set_target(&self, target: &NSObject) {
        // SAFETY: The controller handles these selectors and outlives the window.
        unsafe {
            self.record.setTarget(Some(target));
            self.install.setTarget(Some(target));
            self.save.setTarget(Some(target));
            self.copy.setTarget(Some(target));
            self.dictionary.setTarget(Some(target));
        }
    }

    pub(super) fn preferences(&self) -> Option<Preferences> {
        Some(Preferences {
            language: *Language::OPTIONS
                .get(usize::try_from(self.language.indexOfSelectedItem()).ok()?)?,
            shortcut: *Shortcut::OPTIONS
                .get(usize::try_from(self.shortcut.indexOfSelectedItem()).ok()?)?,
        })
    }

    pub(super) fn update(&self, title: &NSString, status: &NSString, idle: bool, recording: bool) {
        self.record.setTitle(title);
        self.record.setEnabled(idle || recording);
        self.install.setEnabled(idle);
        self.language.setEnabled(idle);
        self.shortcut.setEnabled(idle);
        self.save.setEnabled(idle);
        self.status.setStringValue(status);
        let color = if recording {
            NSColor::systemRedColor()
        } else {
            NSColor::secondaryLabelColor()
        };
        self.status.setTextColor(Some(&color));
    }

    pub(super) fn set_transcript(&self, text: &str) {
        self.placeholder.setHidden(true);
        self.transcript.setString(&NSString::from_str(text));
        self.transcript
            .scrollRangeToVisible(objc2_foundation::NSRange::new(0, 0));
        self.copy.setEnabled(true);
    }

    pub(super) fn transcript(&self) -> String {
        self.transcript.string().to_string()
    }
}

pub(super) fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

pub(super) fn label(main: MainThreadMarker, title: &str, frame: NSRect) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(title), main);
    label.setFrame(frame);
    label
}

fn popup(
    main: MainThreadMarker,
    options: impl IntoIterator<Item = &'static str>,
    selected: usize,
    frame: NSRect,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(main), frame, false);
    for option in options {
        popup.addItemWithTitle(&NSString::from_str(option));
    }
    popup.selectItemAtIndex(selected as isize);
    popup
}

pub(super) fn button(
    main: MainThreadMarker,
    title: &str,
    action: Sel,
    frame: NSRect,
) -> Retained<NSButton> {
    // SAFETY: The controller registers this selector before the button can be used.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            None,
            Some(action),
            main,
        )
    };
    button.setFrame(frame);
    button.setBezelStyle(NSBezelStyle::Push);
    button.setControlSize(NSControlSize::Large);
    button
}
