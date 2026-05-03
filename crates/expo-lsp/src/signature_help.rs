//! Signature help provider for the Expo LSP.
//!
//! When the cursor is inside a function or method call's argument list,
//! displays the parameter names and types with the active parameter
//! highlighted. Supports both free functions (`print(...)`) and method
//! calls (`socket.connect(...)`, `Socket.new(...)`).

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::ExprKind;
use expo_ast::types::Type;
use expo_typecheck::context::{FunctionSig, TypeContext};

use crate::backend::Backend;
use crate::lookup::find_enclosing_call;

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

        let line = pos.line + 1;
        let col = pos.character + 1;
        let call_site = match find_enclosing_call(&state.file, line, col) {
            Some(c) => c,
            None => return Ok(None),
        };

        let (function_name, sig) = match &call_site.expr.kind {
            ExprKind::Call { callee, .. } => {
                let ExprKind::Ident { name, .. } = &callee.kind else {
                    return Ok(None);
                };
                let sig = find_function_sig(name, &state.ctx, &self.stdlib_ctx);
                (name.clone(), sig)
            }
            ExprKind::MethodCall {
                receiver, method, ..
            } => {
                let sig = find_method_sig(receiver, method, &state.ctx, &self.stdlib_ctx);
                (method.clone(), sig)
            }
            _ => return Ok(None),
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
            function_name,
            params_str.join(", "),
            sig.return_type.display()
        );

        let active_param = call_site.active_param as u32;
        let signature = SignatureInformation {
            label,
            documentation: None,
            parameters: Some(params),
            active_parameter: Some(active_param),
        };

        Ok(Some(SignatureHelp {
            signatures: vec![signature],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        }))
    }
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

/// Looks up a method signature using the receiver's `resolved_type` to
/// find the owning type, then retrieves the method from that type's
/// function table.
fn find_method_sig<'a>(
    receiver: &expo_ast::ast::Expr,
    method: &str,
    ctx: &'a TypeContext,
    stdlib_ctx: &'a TypeContext,
) -> Option<&'a FunctionSig> {
    let type_name = receiver_type_name(receiver, ctx)?;
    ctx.find_type(&type_name)
        .and_then(|ti| ti.functions.get(method))
        .or_else(|| {
            stdlib_ctx
                .find_type(&type_name)
                .and_then(|ti| ti.functions.get(method))
        })
        .or_else(|| {
            let mangled = format!("{type_name}_{method}");
            ctx.functions
                .get(&mangled)
                .or_else(|| stdlib_ctx.functions.get(&mangled))
        })
}

/// Extracts the base type name from a receiver expression, preferring
/// `resolved_type` and falling back to ident-based struct/enum lookup.
fn receiver_type_name(receiver: &expo_ast::ast::Expr, ctx: &TypeContext) -> Option<String> {
    if let Some(ty) = &receiver.resolved_type {
        return match ty {
            Type::Named { identifier, .. } => Some(identifier.name.clone()),
            Type::Primitive(p) => Some(p.display().to_string()),
            _ => None,
        };
    }
    if let ExprKind::Ident { name, .. } = &receiver.kind
        && (ctx.is_struct(name) || ctx.is_enum(name))
    {
        return Some(name.clone());
    }
    None
}
