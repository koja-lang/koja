//! The server's shared state and the scheduling of analysis runs.
//! The [`LanguageServer`](tower_lsp_server::LanguageServer) impl
//! lives in `server`, the per-document cache in `document`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::RwLock;
use tower_lsp_server::Client;
use tower_lsp_server::ls_types::*;

use crate::buffer::Buffers;
use crate::convert::PositionEncoding;
use crate::diagnostics::Published;
use crate::document::DocumentState;

/// How long after the last edit to wait before analyzing, so a burst
/// of keystrokes costs one run instead of one per key.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// What `initialize` settled with the client.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Negotiated {
    pub(crate) encoding: PositionEncoding,
    /// Whether the client lets the server register file watchers.
    pub(crate) watches_files: bool,
}

/// The Koja language server backend.
///
/// Holds shared state (open documents and their analyses) and the
/// LSP client handle used to push diagnostics and notifications.
/// Every field is shared, so a clone is a handle onto the same
/// server for a spawned task.
#[derive(Clone)]
pub struct Backend {
    pub(crate) client: Client,
    /// The live text of every open document.
    pub(crate) buffers: Arc<Buffers>,
    /// The last completed analysis of every open document.
    pub(crate) documents: Arc<RwLock<HashMap<Uri, DocumentState>>>,
    /// URIs holding published diagnostics, for stale clearing.
    pub(crate) published: Arc<Published>,
    pub(crate) negotiated: Arc<OnceLock<Negotiated>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish()
    }
}

impl Backend {
    /// Creates a new backend. Every diagnostic run loads its bundle
    /// fresh through `koja_project`, stdlib included, so there is no
    /// cached source state here.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            buffers: Arc::new(Buffers::default()),
            documents: Arc::new(RwLock::new(HashMap::new())),
            published: Arc::new(Published::default()),
            negotiated: Arc::new(OnceLock::new()),
        }
    }

    /// The character encoding settled at `initialize`. UTF-16 until
    /// then, which only matters for a client that skips the handshake.
    pub(crate) fn encoding(&self) -> PositionEncoding {
        self.negotiated
            .get()
            .map_or(PositionEncoding::Utf16, |negotiated| negotiated.encoding)
    }

    /// Analyze `uri` once the editor has been quiet for [`DEBOUNCE`].
    /// `revision` is the buffer revision that asked for the run. A
    /// newer revision by the time the wait ends means a later change
    /// scheduled its own run, so this one stops.
    pub(crate) fn schedule_diagnose(&self, uri: Uri, revision: u64) {
        let backend = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if backend.buffers.revision(&uri) == Some(revision) {
                backend.diagnose(uri).await;
            }
        });
    }

    /// Analyze every open document again after something outside
    /// them changed, each through its own debounce.
    pub(crate) fn rediagnose_open_documents(&self) {
        for uri in self.buffers.open_uris() {
            if let Some(revision) = self.buffers.touch(&uri) {
                self.schedule_diagnose(uri, revision);
            }
        }
    }

    /// Ask the client to report `.koja` and `koja.toml` changes on
    /// disk, so a sibling edited outside the editor refreshes the
    /// diagnostics of the files that depend on it.
    pub(crate) async fn register_file_watchers(&self) {
        let watchers = ["**/*.koja", "**/koja.toml"]
            .into_iter()
            .map(|pattern| FileSystemWatcher {
                glob_pattern: GlobPattern::String(pattern.to_string()),
                kind: None,
            })
            .collect();
        let options = DidChangeWatchedFilesRegistrationOptions { watchers };
        let registration = Registration {
            id: "koja-lsp/watched-files".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: serde_json::to_value(options).ok(),
        };
        if let Err(err) = self.client.register_capability(vec![registration]).await {
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("koja-lsp could not register file watchers: {err}"),
                )
                .await;
        }
    }
}
