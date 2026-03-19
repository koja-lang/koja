//! Signature help provider for the Expo LSP.
//!
//! When the cursor is inside a function call's argument list, displays
//! the function's parameter names and types with the active parameter
//! highlighted.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_typecheck::context::{FunctionSig, TypeContext};

use crate::backend::Backend;

impl Backend {
    /// Handles `textDocument/signatureHelp` requests by finding the
    /// function call surrounding the cursor and returning its signature.
    pub(crate) async fn handle_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };

        let call = match find_call_context(&state.source, pos) {
            Some(c) => c,
            None => return Ok(None),
        };

        let sig = find_function_sig(&call.function_name, &state.ctx, &self.stdlib_ctx);
        let sig = match sig {
            Some(s) => s,
            None => return Ok(None),
        };

        let params: Vec<ParameterInformation> = sig
            .params
            .iter()
            .map(|p| ParameterInformation {
                label: ParameterLabel::Simple(format!("{}: {}", p.name, p.ty.display())),
                documentation: None,
            })
            .collect();

        let params_str: Vec<String> = sig
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty.display()))
            .collect();
        let label = format!(
            "fn {}({}) -> {}",
            call.function_name,
            params_str.join(", "),
            sig.return_type.display()
        );

        let signature = SignatureInformation {
            label,
            documentation: None,
            parameters: Some(params),
            active_parameter: Some(call.active_param),
        };

        Ok(Some(SignatureHelp {
            signatures: vec![signature],
            active_signature: Some(0),
            active_parameter: Some(call.active_param),
        }))
    }
}

/// Context about a function call at the cursor position.
struct CallContext {
    function_name: String,
    active_param: u32,
}

/// Scans the source text backwards from the cursor to find the enclosing
/// function call and determine which parameter is active.
fn find_call_context(source: &str, pos: Position) -> Option<CallContext> {
    let lines: Vec<&str> = source.lines().collect();
    let line_idx = pos.line as usize;
    if line_idx >= lines.len() {
        return None;
    }

    let col = pos.character as usize;
    let current_line = lines[line_idx];
    let col = col.min(current_line.len());

    let mut flat_offset = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if i == line_idx {
            flat_offset += col;
            break;
        }
        flat_offset += line.len() + 1;
    }

    let bytes = source.as_bytes();
    if flat_offset > bytes.len() {
        return None;
    }

    let mut depth = 0i32;
    let mut commas = 0u32;
    let mut paren_pos = None;

    let mut i = flat_offset;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                if depth == 0 {
                    paren_pos = Some(i);
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => commas += 1,
            _ => {}
        }
    }

    let paren_pos = paren_pos?;

    let name_end = paren_pos;
    let mut name_start = name_end;
    while name_start > 0 {
        let c = bytes[name_start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            name_start -= 1;
        } else {
            break;
        }
    }

    if name_start == name_end {
        return None;
    }

    let function_name = std::str::from_utf8(&bytes[name_start..name_end])
        .ok()?
        .to_string();

    Some(CallContext {
        function_name,
        active_param: commas,
    })
}

/// Looks up a function signature by name, checking the document's context
/// first, then the stdlib.
fn find_function_sig<'a>(
    name: &str,
    ctx: &'a TypeContext,
    stdlib_ctx: &'a TypeContext,
) -> Option<&'a FunctionSig> {
    ctx.functions
        .get(name)
        .or_else(|| stdlib_ctx.functions.get(name))
}
