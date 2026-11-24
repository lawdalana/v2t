mod store;

use std::cell::RefCell;

use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSBorderType, NSButton,
    NSControlTextEditingDelegate, NSScrollView, NSTableColumn, NSTableView, NSTableViewDataSource,
    NSTableViewDelegate, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSString};

use super::{
    Storage,
    window::{button, label, rect},
};
use crate::text::Entry;
use store::{Change, Dictionary};

const SPOKEN_COLUMN: &str = "spoken";
const WIDTH: f64 = 600.0;
const HEIGHT: f64 = 420.0;
const MARGIN: f64 = 24.0;
const CONTENT_WIDTH: f64 = WIDTH - MARGIN * 2.0;

pub(super) struct DictionaryControls {
    window: Retained<NSWindow>,
    table: Retained<NSTableView>,
    spoken: Retained<NSTextField>,
    replacement: Retained<NSTextField>,
    save: Retained<NSButton>,
    remove: Retained<NSButton>,
    new: Retained<NSButton>,
    status: Retained<NSTextField>,
    dictionary: RefCell<Option<Dictionary>>,
}

define_class!(
    // SAFETY: AppKit invokes the controls and table delegates on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DictionaryControls]
    pub(super) struct DictionaryEditor;

    unsafe impl NSObjectProtocol for DictionaryEditor {}
    unsafe impl NSControlTextEditingDelegate for DictionaryEditor {}

    #[allow(non_snake_case)]
    unsafe impl NSTableViewDataSource for DictionaryEditor {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn numberOfRowsInTableView(&self, _table: &NSTableView) -> isize {
            self.ivars().dictionary.borrow().as_ref()
                .map_or(0, |dictionary| dictionary.entries().len() as isize)
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn tableView_objectValueForTableColumn_row(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<AnyObject>> {
            self.cell_value(column, row)
        }
    }

    #[allow(non_snake_case)]
    unsafe impl NSTableViewDelegate for DictionaryEditor {
        #[unsafe(method(selectionShouldChangeInTableView:))]
        fn selectionShouldChangeInTableView(&self, _table: &NSTableView) -> bool {
            let has_changes = self.form_has_changes();
            if has_changes {
                self.ivars().status.setStringValue(&NSString::from_str(
                    "Save the current rule or click New to clear the form before selecting another row.",
                ));
            }
            !has_changes
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn tableViewSelectionDidChange(&self, _notification: &NSNotification) {
            self.update_form();
        }
    }

    impl DictionaryEditor {
        #[unsafe(method(newDictionaryEntry:))]
        fn new_entry(&self, _sender: &NSButton) {
            self.reset_form();
        }

        #[unsafe(method(saveDictionaryEntry:))]
        fn save_entry(&self, _sender: &NSButton) {
            if !self.ivars().window.makeFirstResponder(None) {
                return;
            }
            let entry = Entry {
                spoken: self.ivars().spoken.stringValue().to_string(),
                replacement: self.ivars().replacement.stringValue().to_string(),
            };
            match self.selected_row() {
                Some(row) => self.apply(Change::Update { row, entry }, "Rule updated."),
                None => self.apply(Change::Add(entry), "Rule added."),
            }
        }

        #[unsafe(method(removeDictionaryEntry:))]
        fn remove_entry(&self, _sender: &NSButton) {
            if let Some(row) = self.selected_row() {
                self.apply(Change::Remove { row }, "Rule removed.");
            }
        }
    }
);

impl DictionaryEditor {
    pub(super) fn new(main: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(main).set_ivars(DictionaryControls::new(main));
        // SAFETY: The Rust ivars are already initialized.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let controls = this.ivars();
        // SAFETY: The controller keeps the editor alive while AppKit uses these weak references.
        unsafe {
            controls
                .table
                .setDataSource(Some(ProtocolObject::from_ref(&*this)));
            controls
                .table
                .setDelegate(Some(ProtocolObject::from_ref(&*this)));
            for button in [&controls.new, &controls.save, &controls.remove] {
                button.setTarget(Some(this.as_super()));
            }
        }
        this
    }

    #[allow(deprecated)] // Supports activation on macOS 12.
    pub(super) fn show(&self) {
        let controls = self.ivars();
        if !controls.window.isVisible() {
            match Storage::resolve().and_then(|storage| Ok(Dictionary::load(storage.config)?)) {
                Ok(dictionary) => {
                    controls.dictionary.replace(Some(dictionary));
                    controls.status.setStringValue(&NSString::from_str(
                        "Changes apply to the next transcription.",
                    ));
                }
                Err(error) => {
                    controls.dictionary.replace(None);
                    controls
                        .status
                        .setStringValue(&NSString::from_str(&error.to_string()));
                }
            }
            controls.table.reloadData();
            self.reset_form();
        }
        controls.window.makeKeyAndOrderFront(None);
        NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
    }

    fn selected_row(&self) -> Option<usize> {
        self.ivars().table.selectedRow().try_into().ok()
    }

    fn cell_value(
        &self,
        column: Option<&NSTableColumn>,
        row: isize,
    ) -> Option<Retained<AnyObject>> {
        let dictionary = self.ivars().dictionary.borrow();
        let entry = dictionary
            .as_ref()?
            .entries()
            .get(usize::try_from(row).ok()?)?;
        let value = if column?.identifier().to_string() == SPOKEN_COLUMN {
            &entry.spoken
        } else {
            &entry.replacement
        };
        Some(Retained::into_super(Retained::into_super(
            NSString::from_str(value),
        )))
    }

    fn reset_form(&self) {
        if let Some(row) = self.selected_row() {
            self.update_form();
            self.ivars().table.deselectRow(row as isize);
        }
        self.update_form();
        self.ivars()
            .window
            .makeFirstResponder(Some(&self.ivars().spoken));
    }

    fn form_has_changes(&self) -> bool {
        let controls = self.ivars();
        let dictionary = controls.dictionary.borrow();
        let chosen = self
            .selected_row()
            .and_then(|row| dictionary.as_ref()?.entries().get(row));
        let text = |field: &NSTextField| {
            field
                .currentEditor()
                .map_or_else(|| field.stringValue(), |editor| editor.string())
                .to_string()
        };
        text(&controls.spoken) != chosen.map_or("", |entry| &entry.spoken)
            || text(&controls.replacement) != chosen.map_or("", |entry| &entry.replacement)
    }

    fn update_form(&self) {
        let controls = self.ivars();
        let dictionary = controls.dictionary.borrow();
        let chosen = self
            .selected_row()
            .and_then(|row| dictionary.as_ref()?.entries().get(row));
        let available = dictionary.is_some();
        let spoken = NSString::from_str(chosen.map_or("", |entry| &entry.spoken));
        let replacement = NSString::from_str(chosen.map_or("", |entry| &entry.replacement));
        let editing = chosen.is_some();
        drop(dictionary);
        controls.spoken.setStringValue(&spoken);
        controls.replacement.setStringValue(&replacement);
        controls.spoken.setEnabled(available);
        controls.replacement.setEnabled(available);
        controls.new.setEnabled(available);
        controls.save.setEnabled(available);
        controls.remove.setEnabled(editing);
        controls
            .save
            .setTitle(&NSString::from_str(if editing { "Update" } else { "Add" }));
    }

    fn apply(&self, change: Change, success: &str) {
        let result = self
            .ivars()
            .dictionary
            .borrow_mut()
            .as_mut()
            .map(|dictionary| dictionary.change(change));
        let text = match result {
            Some(Ok(())) => {
                self.ivars().table.reloadData();
                self.reset_form();
                NSString::from_str(success)
            }
            Some(Err(error)) => NSString::from_str(&error.to_string()),
            None => return,
        };
        self.ivars().status.setStringValue(&text);
    }
}

impl DictionaryControls {
    fn new(main: MainThreadMarker) -> Self {
        let bounds = rect(0.0, 0.0, WIDTH, HEIGHT);
        // SAFETY: The editor owns the window while it is closed or open.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(main),
                bounds,
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str("Dictionary"));
        window.center();

        let view = NSView::initWithFrame(NSView::alloc(main), bounds);
        let column_width = (CONTENT_WIDTH - 12.0) / 2.0;
        let replacement_x = MARGIN + column_width + 12.0;
        let spoken_label = label(main, "Spoken text", rect(MARGIN, 376.0, column_width, 20.0));
        let replacement_label = label(
            main,
            "Replace with",
            rect(replacement_x, 376.0, column_width, 20.0),
        );
        let spoken = NSTextField::textFieldWithString(&NSString::from_str(""), main);
        spoken.setFrame(rect(MARGIN, 344.0, column_width, 26.0));
        let replacement = NSTextField::textFieldWithString(&NSString::from_str(""), main);
        replacement.setFrame(rect(replacement_x, 344.0, column_width, 26.0));

        let new = button(
            main,
            "New",
            sel!(newDictionaryEntry:),
            rect(MARGIN, 298.0, 90.0, 34.0),
        );
        let remove = button(
            main,
            "Remove",
            sel!(removeDictionaryEntry:),
            rect(126.0, 298.0, 115.0, 34.0),
        );
        let save = button(
            main,
            "Add",
            sel!(saveDictionaryEntry:),
            rect(458.0, 298.0, 118.0, 34.0),
        );
        save.setKeyEquivalent(&NSString::from_str("\r"));

        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(main),
            rect(MARGIN, 60.0, CONTENT_WIDTH, 224.0),
        );
        scroll.setBorderType(NSBorderType::BezelBorder);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        let table = NSTableView::initWithFrame(
            NSTableView::alloc(main),
            NSRect::new(NSPoint::new(0.0, 0.0), scroll.contentSize()),
        );
        table.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        table.setUsesAlternatingRowBackgroundColors(true);
        table.setAllowsColumnReordering(false);
        table.setAllowsEmptySelection(true);
        table.setRowHeight(24.0);
        for (identifier, title) in [
            (SPOKEN_COLUMN, "Spoken text"),
            ("replacement", "Replace with"),
        ] {
            let column = NSTableColumn::initWithIdentifier(
                NSTableColumn::alloc(main),
                &NSString::from_str(identifier),
            );
            column.setTitle(&NSString::from_str(title));
            column.setWidth((scroll.contentSize().width - 8.0) / 2.0);
            column.setEditable(false);
            table.addTableColumn(&column);
        }
        scroll.setDocumentView(Some(&table));

        let status = NSTextField::wrappingLabelWithString(&NSString::from_str(""), main);
        status.setFrame(rect(MARGIN, 16.0, CONTENT_WIDTH, 34.0));
        for child in [
            &*spoken_label as &NSView,
            &*replacement_label,
            &*spoken,
            &*replacement,
            &*new,
            &*remove,
            &*save,
            &*scroll,
            &*status,
        ] {
            view.addSubview(child);
        }
        window.setContentView(Some(&view));

        Self {
            window,
            table,
            spoken,
            replacement,
            save,
            remove,
            new,
            status,
            dictionary: RefCell::new(None),
        }
    }
}
