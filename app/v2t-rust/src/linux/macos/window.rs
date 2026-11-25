use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained, runtime::Sel, sel};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle, NSBorderType, NSButton, NSColor,
    NSControlSize, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSPopUpButton,
    NSScrollView, NSTextField, NSTextView, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};

use super::{
    READY,
    preferences::{CorrectionProvider, Language, Preferences, Shortcut},
    right_option,
};

const WIDTH: f64 = 508.0;
const SHORTCUT_ROW_HEIGHT: f64 = 24.0;
const TOP_SHORTCUT_Y: f64 = 360.0 + (Shortcut::OPTIONS.len() - 1) as f64 * SHORTCUT_ROW_HEIGHT;
const HEIGHT: f64 = TOP_SHORTCUT_Y + 140.0;
const MARGIN: f64 = 24.0;
const CONTENT_WIDTH: f64 = WIDTH - MARGIN * 2.0;
const FIELD_X: f64 = 181.0;
const FIELD_WIDTH: f64 = WIDTH - MARGIN - FIELD_X;

pub(super) struct MainWindow {
    pub(super) window: Retained<NSWindow>,
    status: Retained<NSTextField>,
    language: Retained<NSPopUpButton>,
    shortcuts: [Retained<NSButton>; Shortcut::OPTIONS.len()],
    shortcut_accessibility: Retained<NSButton>,
    ai_correction: Retained<NSPopUpButton>,
    install_correction: Retained<NSButton>,
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
            rect(MARGIN, HEIGHT - 61.0, CONTENT_WIDTH - 126.0, 38.0),
        );
        title.setFont(Some(&NSFont::boldSystemFontOfSize(29.0)));
        let dictionary = button(
            main,
            "Dictionary…",
            sel!(showDictionary:),
            rect(WIDTH - MARGIN - 114.0, HEIGHT - 59.0, 114.0, 34.0),
        );
        let language_label = label(
            main,
            "Dictation language",
            rect(MARGIN, HEIGHT - 98.0, 150.0, 20.0),
        );
        let language = popup(
            main,
            Language::OPTIONS.map(Language::label),
            Language::OPTIONS
                .iter()
                .position(|&item| item == preferences.language)
                .unwrap_or(0),
            rect(FIELD_X, HEIGHT - 103.0, FIELD_WIDTH, 30.0),
        );
        let shortcut_label = label(
            main,
            "Hold to dictate",
            rect(MARGIN, TOP_SHORTCUT_Y + 2.0, 150.0, 20.0),
        );
        let shortcuts = std::array::from_fn(|index| {
            let shortcut = Shortcut::OPTIONS[index];
            // SAFETY: AppKit toggles the checkbox locally until Save Settings is pressed.
            let button = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &NSString::from_str(shortcut.label()),
                    None,
                    None,
                    main,
                )
            };
            button.setFrame(rect(
                FIELD_X,
                TOP_SHORTCUT_Y - index as f64 * SHORTCUT_ROW_HEIGHT,
                FIELD_WIDTH,
                SHORTCUT_ROW_HEIGHT,
            ));
            button.setState(if preferences.shortcuts.contains(shortcut) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            button
        });
        let instructions = label(
            main,
            "Hold any checked shortcut. Release it to copy.",
            rect(FIELD_X, 332.0, FIELD_WIDTH, 20.0),
        );
        instructions.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let shortcut_accessibility = button(
            main,
            "Allow Accessibility…",
            sel!(requestShortcutAccessibility:),
            rect(MARGIN, 326.0, 146.0, 30.0),
        );
        shortcut_accessibility.setControlSize(NSControlSize::Small);
        shortcut_accessibility.setHidden(true);

        let correction_label = label(main, "AI correction", rect(MARGIN, 291.0, 98.0, 20.0));
        let selected = match (preferences.ai_correction, preferences.correction_provider) {
            (false, _) => 0,
            (true, CorrectionProvider::Local) => 1,
            (true, CorrectionProvider::LiteLlm) => 2,
        };
        let ai_correction = popup(
            main,
            ["Off", "Typhoon 2.5 4B (local)", "LiteLLM"],
            selected,
            rect(122.0, 286.0, 200.0, 30.0),
        );
        let install_correction = button(
            main,
            "Install Local AI",
            sel!(installCorrectionModel:),
            rect(WIDTH - MARGIN - 148.0, 282.0, 148.0, 38.0),
        );

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
        ] {
            view.addSubview(child);
        }
        for shortcut in &shortcuts {
            view.addSubview(shortcut);
        }
        for child in [
            &*instructions as &NSView,
            &*shortcut_accessibility,
            &*correction_label,
            &*ai_correction,
            &*install_correction,
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
            shortcuts,
            shortcut_accessibility,
            ai_correction,
            install_correction,
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
            self.install_correction.setTarget(Some(target));
            self.save.setTarget(Some(target));
            self.copy.setTarget(Some(target));
            self.dictionary.setTarget(Some(target));
            self.shortcut_accessibility.setTarget(Some(target));
        }
    }

    pub(super) fn preferences(&self) -> Option<Preferences> {
        let (ai_correction, correction_provider) = match self.ai_correction.indexOfSelectedItem() {
            0 => (false, CorrectionProvider::Local),
            1 => (true, CorrectionProvider::Local),
            2 => (true, CorrectionProvider::LiteLlm),
            _ => return None,
        };
        Some(Preferences {
            language: *Language::OPTIONS
                .get(usize::try_from(self.language.indexOfSelectedItem()).ok()?)?,
            shortcuts: Shortcut::OPTIONS
                .into_iter()
                .zip(&self.shortcuts)
                .filter_map(|(shortcut, button)| {
                    (button.state() == NSControlStateValueOn).then_some(shortcut)
                })
                .collect(),
            ai_correction,
            correction_provider,
        })
    }

    pub(super) fn set_status(&self, status: &NSString) {
        self.status.setStringValue(status);
    }

    pub(super) fn update(
        &self,
        title: &NSString,
        idle: bool,
        recording: bool,
        right_option_global: Option<bool>,
    ) {
        self.record.setTitle(title);
        self.record.setEnabled(idle || recording);
        self.install.setEnabled(idle);
        self.install_correction.setEnabled(idle);
        self.ai_correction.setEnabled(idle);
        self.language.setEnabled(idle);
        for (shortcut, button) in Shortcut::OPTIONS.into_iter().zip(&self.shortcuts) {
            button.setEnabled(idle);
            if shortcut == Shortcut::RightOptionSpace {
                let title = if right_option_global.unwrap_or_else(right_option::has_accessibility) {
                    NSString::from_str(shortcut.label())
                } else {
                    NSString::from_str(&format!("{} (v2t focus)", shortcut.label()))
                };
                button.setTitle(&title);
            }
        }
        self.shortcut_accessibility
            .setHidden(right_option_global != Some(false));
        self.shortcut_accessibility.setEnabled(idle);
        self.save.setEnabled(idle);
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
