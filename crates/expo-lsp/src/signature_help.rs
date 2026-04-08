//! Signature help provider for the Expo LSP.
//!
//! When the cursor is inside a function or method call's argument list,
//! displays the parameter names and types with the active parameter
//! highlighted. Supports both free functions (`print(...)`) and method
//! calls (`socket.connect(...)`, `Socket.new(...)`).

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_typecheck::context::{FunctionSig, TypeContext};

use crate::backend::Backend;
use crate::lookup::receiver::resolve_receiver_type;

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

        let sig = match &call.receiver {
            Some(receiver) => find_method_sig(
                receiver,
                &call.function_name,
                &state.source,
                &state.ctx,
                &self.stdlib_ctx,
            ),
            None => find_function_sig(&call.function_name, &state.ctx, &self.stdlib_ctx),
        };

        let sig = match sig {
            Some(s) => s,
            None => return Ok(None),
        };

        let params: Vec<ParameterInformation> = sig
            .params
            .iter()
            .filter(|p| p.name != "self")
            .map(|p| ParameterInformation {
                label: ParameterLabel::Simple(format!("{}: {}", p.name, p.ty.display())),
                documentation: None,
            })
            .collect();

        let params_str: Vec<String> = sig
            .params
            .iter()
            .filter(|p| p.name != "self")
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

struct CallContext {
    function_name: String,
    /// If the call is `receiver.method(...)`, the receiver token.
    receiver: Option<String>,
    active_param: u32,
}

/// Scans the source text backwards from the cursor to find the enclosing
/// function call and determine which parameter is active. Also detects
/// method calls by checking for `.` before the function name.
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

    let receiver = if name_start > 0 && bytes[name_start - 1] == b'.' {
        let dot_pos = name_start - 1;
        let recv_end = dot_pos;
        let mut recv_start = recv_end;
        while recv_start > 0 {
            let c = bytes[recv_start - 1];
            if c.is_ascii_alphanumeric() || c == b'_' {
                recv_start -= 1;
            } else {
                break;
            }
        }
        if recv_start < recv_end {
            std::str::from_utf8(&bytes[recv_start..recv_end])
                .ok()
                .map(|s| s.to_string())
        } else {
            None
        }
    } else {
        None
    };

    Some(CallContext {
        function_name,
        receiver,
        active_param: commas,
    })
}

/// Looks up a free function signature by name.
fn find_function_sig<'a>(
    name: &str,
    ctx: &'a TypeContext,
    stdlib_ctx: &'a TypeContext,
) -> Option<&'a FunctionSig> {
    ctx.functions
        .get(name)
        .or_else(|| stdlib_ctx.functions.get(name))
}

/// Looks up a method signature by resolving the receiver type, then
/// searching for the method in that type's functions. Falls back to
/// the mangled `Type_method` name in the global function table.
fn find_method_sig<'a>(
    receiver: &str,
    method: &str,
    source: &str,
    ctx: &'a TypeContext,
    stdlib_ctx: &'a TypeContext,
) -> Option<&'a FunctionSig> {
    if let Some(type_name) = resolve_receiver_type(receiver, source, ctx)
        .or_else(|| resolve_receiver_type(receiver, source, stdlib_ctx))
    {
        if let Some(sig) = ctx
            .find_type(&type_name)
            .and_then(|ti| ti.functions.get(method))
        {
            return Some(sig);
        }
        if let Some(sig) = stdlib_ctx
            .find_type(&type_name)
            .and_then(|ti| ti.functions.get(method))
        {
            return Some(sig);
        }
    }

    let mangled = format!("{}_{}", receiver, method);
    ctx.functions
        .get(&mangled)
        .or_else(|| stdlib_ctx.functions.get(&mangled))
        .or_else(|| {
            let suffix = format!("_{}", method);
            ctx.functions
                .iter()
                .find(|(k, _)| k.ends_with(&suffix))
                .map(|(_, v)| v)
                .or_else(|| {
                    stdlib_ctx
                        .functions
                        .iter()
                        .find(|(k, _)| k.ends_with(&suffix))
                        .map(|(_, v)| v)
                })
        })
}
