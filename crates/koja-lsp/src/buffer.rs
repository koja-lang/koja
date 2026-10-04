//! The text of every open document, kept in step with the editor.
//!
//! A change applies inside the notification handler before its first
//! `await`. `tower-lsp-server` dispatches handlers through
//! `buffer_unordered`, which polls each new handler once in arrival
//! order, so a change that finishes in that first poll lands in the
//! order the editor sent it. The lock is a `std` mutex for that
//! reason. An `await` on an async lock would hand the slot to the
//! next handler and lose the order.
//!
//! The analysis in `DocumentState` lags behind the buffer by one
//! debounce window. Handlers that need the text as the editor has it
//! right now, such as formatting, read a snapshot from here instead.

use std::collections::HashMap;
use std::sync::Mutex;

use tower_lsp_server::ls_types::*;

use crate::convert::{PositionEncoding, Positions};

/// One open document.
#[derive(Debug, Clone)]
pub(crate) struct Buffer {
    /// Counts every reason to analyze again, so a debounced run can
    /// tell whether it is still the latest. Edits bump it, and so does
    /// a sibling file changing on disk.
    pub(crate) revision: u64,
    pub(crate) text: String,
    /// The editor's version for the text, echoed on published
    /// diagnostics so the editor can drop a stale set.
    pub(crate) version: i32,
}

impl Buffer {
    /// Apply one content change. A change without a range replaces
    /// the whole text.
    fn apply(&mut self, change: TextDocumentContentChangeEvent, encoding: PositionEncoding) {
        let Some(range) = change.range else {
            self.text = change.text;
            return;
        };
        let positions = Positions::new(encoding, &self.text);
        let start = positions.offset(range.start);
        let end = positions.offset(range.end).max(start);
        self.text.replace_range(start..end, &change.text);
    }
}

#[derive(Debug, Default)]
pub(crate) struct Buffers {
    inner: Mutex<HashMap<String, Buffer>>,
}

impl Buffers {
    /// Record a newly opened document.
    pub(crate) fn open(&self, uri: &str, text: String, version: i32) {
        self.lock().insert(
            uri.to_string(),
            Buffer {
                revision: 0,
                text,
                version,
            },
        );
    }

    /// Apply the changes of one `didChange`. Returns the new revision,
    /// or `None` when the document is not open or `version` is not
    /// newer than the text held, in which case nothing changes.
    pub(crate) fn change(
        &self,
        uri: &str,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
        encoding: PositionEncoding,
    ) -> Option<u64> {
        let mut buffers = self.lock();
        let buffer = buffers.get_mut(uri)?;
        if version <= buffer.version {
            return None;
        }
        for change in changes {
            buffer.apply(change, encoding);
        }
        buffer.version = version;
        buffer.revision += 1;
        Some(buffer.revision)
    }

    /// Bump the revision without changing the text, when something
    /// outside the document (a sibling on disk, a save) calls for a
    /// fresh analysis. Returns the new revision.
    pub(crate) fn touch(&self, uri: &str) -> Option<u64> {
        let mut buffers = self.lock();
        let buffer = buffers.get_mut(uri)?;
        buffer.revision += 1;
        Some(buffer.revision)
    }

    pub(crate) fn close(&self, uri: &str) {
        self.lock().remove(uri);
    }

    /// A copy of the document as it stands.
    pub(crate) fn snapshot(&self, uri: &str) -> Option<Buffer> {
        self.lock().get(uri).cloned()
    }

    pub(crate) fn revision(&self, uri: &str) -> Option<u64> {
        self.lock().get(uri).map(|buffer| buffer.revision)
    }

    pub(crate) fn is_open(&self, uri: &str) -> bool {
        self.lock().contains_key(uri)
    }

    pub(crate) fn open_uris(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// The URI and text of every open document.
    pub(crate) fn open_texts(&self) -> Vec<(String, String)> {
        self.lock()
            .iter()
            .map(|(uri, buffer)| (uri.clone(), buffer.text.clone()))
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Buffer>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "file:///proj/src/main.koja";

    fn change(
        range: Option<((u32, u32), (u32, u32))>,
        text: &str,
    ) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: range.map(|((l1, c1), (l2, c2))| {
                Range::new(Position::new(l1, c1), Position::new(l2, c2))
            }),
            range_length: None,
            text: text.to_string(),
        }
    }

    fn text(buffers: &Buffers) -> String {
        buffers.snapshot(URI).unwrap().text
    }

    #[test]
    fn ranged_changes_apply_in_order() {
        let buffers = Buffers::default();
        buffers.open(URI, "fn run\n  1\nend\n".to_string(), 1);
        let revision = buffers.change(
            URI,
            2,
            vec![
                change(Some(((1, 2), (1, 3))), "42"),
                change(Some(((0, 3), (0, 6))), "main"),
            ],
            PositionEncoding::Utf16,
        );
        assert_eq!(revision, Some(1));
        assert_eq!(text(&buffers), "fn main\n  42\nend\n");
    }

    #[test]
    fn full_change_replaces_the_text() {
        let buffers = Buffers::default();
        buffers.open(URI, "old".to_string(), 1);
        buffers.change(URI, 2, vec![change(None, "new")], PositionEncoding::Utf16);
        assert_eq!(text(&buffers), "new");
    }

    #[test]
    fn ranges_use_the_negotiated_encoding() {
        let buffers = Buffers::default();
        buffers.open(URI, "x = \"😀\" + y\n".to_string(), 1);
        // `y` is UTF-16 unit 11 and byte 13.
        buffers.change(
            URI,
            2,
            vec![change(Some(((0, 11), (0, 12))), "z")],
            PositionEncoding::Utf16,
        );
        assert_eq!(text(&buffers), "x = \"😀\" + z\n");
        buffers.change(
            URI,
            3,
            vec![change(Some(((0, 13), (0, 14))), "w")],
            PositionEncoding::Utf8,
        );
        assert_eq!(text(&buffers), "x = \"😀\" + w\n");
    }

    #[test]
    fn stale_versions_are_dropped() {
        let buffers = Buffers::default();
        buffers.open(URI, "a".to_string(), 5);
        assert_eq!(
            buffers.change(URI, 5, vec![change(None, "b")], PositionEncoding::Utf16),
            None
        );
        assert_eq!(
            buffers.change(URI, 4, vec![change(None, "c")], PositionEncoding::Utf16),
            None
        );
        assert_eq!(text(&buffers), "a");
        assert_eq!(buffers.revision(URI), Some(0));
    }

    #[test]
    fn touch_bumps_the_revision_only() {
        let buffers = Buffers::default();
        buffers.open(URI, "a".to_string(), 1);
        assert_eq!(buffers.touch(URI), Some(1));
        let buffer = buffers.snapshot(URI).unwrap();
        assert_eq!((buffer.text.as_str(), buffer.version), ("a", 1));
        assert_eq!(buffers.touch("file:///nowhere"), None);
    }

    #[test]
    fn close_forgets_the_document() {
        let buffers = Buffers::default();
        buffers.open(URI, "a".to_string(), 1);
        assert!(buffers.is_open(URI));
        buffers.close(URI);
        assert!(!buffers.is_open(URI));
        assert!(buffers.open_uris().is_empty());
    }
}
