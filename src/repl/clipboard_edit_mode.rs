use crate::clipboard::{self, PastedImage};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use reedline::{EditCommand, EditMode, Emacs, PromptEditMode, ReedlineEvent, ReedlineRawEvent};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Wraps the Emacs edit mode to intercept Ctrl+V and save clipboard images
/// at the moment they're pasted, with unique numbered markers (①②③...).
pub struct ClipboardEditMode {
    inner: Emacs,
    images: Arc<Mutex<Vec<PastedImage>>>,
    counter: Arc<AtomicUsize>,
}

impl ClipboardEditMode {
    pub fn new(inner: Emacs) -> Self {
        Self {
            inner,
            images: Arc::new(Mutex::new(Vec::new())),
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn images_handle(&self) -> Arc<Mutex<Vec<PastedImage>>> {
        self.images.clone()
    }

    pub fn counter_handle(&self) -> Arc<AtomicUsize> {
        self.counter.clone()
    }
}

impl EditMode for ClipboardEditMode {
    fn parse_event(&mut self, event: ReedlineRawEvent) -> ReedlineEvent {
        let raw: Event = event.into();

        if let Event::Key(KeyEvent {
            code: KeyCode::Char('v'),
            modifiers,
            ..
        }) = &raw
        {
            if modifiers.contains(KeyModifiers::CONTROL) {
                if let Some(image) = clipboard::get_clipboard_image() {
                    let idx = self.counter.fetch_add(1, Ordering::SeqCst);
                    let marker = clipboard::marker_for_index(idx);
                    if let Ok(mut imgs) = self.images.lock() {
                        imgs.push(image);
                    }
                    return ReedlineEvent::Edit(vec![EditCommand::InsertString(format!(
                        "{} ",
                        marker
                    ))]);
                }
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if let Ok(text) = cb.get_text() {
                        if !text.is_empty() {
                            return ReedlineEvent::Edit(vec![EditCommand::InsertString(text)]);
                        }
                    }
                }
                return ReedlineEvent::None;
            }
        }

        match ReedlineRawEvent::try_from(raw) {
            Ok(ev) => self.inner.parse_event(ev),
            Err(_) => ReedlineEvent::None,
        }
    }

    fn edit_mode(&self) -> PromptEditMode {
        self.inner.edit_mode()
    }
}
