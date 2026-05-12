//! Zed extension entry point for the Expo language.
//!
//! Wires up two things on top of the tree-sitter grammar:
//!   1. The `expo-lsp` language server, looked up on `$PATH` by default
//!      and overridable via Zed's per-language LSP settings.
//!   2. The `Expo` language registration so the grammar's queries are
//!      attached to `.expo` files.

use zed_extension_api::{
    self as zed, settings::LspSettings, Command, LanguageServerId, Result, Worktree,
};

const LSP_NAME: &str = "expo-lsp";

pub struct ExpoExtension;

impl zed::Extension for ExpoExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let binary = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let command = binary
            .as_ref()
            .and_then(|cmd| cmd.path.clone())
            .or_else(|| worktree.which(LSP_NAME))
            .ok_or_else(|| {
                format!(
                    "couldn't find `{LSP_NAME}` on $PATH. Install it (e.g. `cargo install \
                     --path crates/expo-lsp` from the Expo workspace) or set \
                     `lsp.expo-lsp.binary.path` in your Zed settings."
                )
            })?;

        let args = binary
            .as_ref()
            .and_then(|cmd| cmd.arguments.clone())
            .unwrap_or_default();
        let env = binary
            .and_then(|cmd| cmd.env)
            .map(|env| env.into_iter().collect())
            .unwrap_or_default();

        Ok(Command { command, args, env })
    }
}

zed::register_extension!(ExpoExtension);
