//! Signature help provider for the Expo LSP.
//!
//! When the cursor is inside a function or method call's argument list,
//! displays the parameter names and types with the active parameter
//! highlighted. Supports both free functions and method calls.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_alpha_typecheck::{FunctionSignature, GlobalKind, GlobalRegistry};
use expo_ast::ast::ExprKind;
use expo_ast::identifier::Identifier;

use crate::alpha_format::format_resolved_type;
use crate::backend::Backend;
use crate::lookup::{LookupCtx, find_enclosing_call, traverse_receiver_type_id};

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
        let (file, registry) = match (state.active_file(), state.registry()) {
            (Some(f), Some(r)) => (f, r),
            _ => return Ok(None),
        };

        let ctx = LookupCtx {
            registry,
            package: &state.active_package,
            locals: &state.locals,
        };

        let line = pos.line + 1;
        let col = pos.character + 1;
        let call_site = match find_enclosing_call(file, line, col) {
            Some(c) => c,
            None => return Ok(None),
        };

        let (function_name, sig) = match &call_site.expr.kind {
            ExprKind::Call { callee, .. } => {
                let ExprKind::Ident { name, .. } = &callee.kind else {
                    return Ok(None);
                };
                let sig = find_function_sig(name, &state.active_package, registry);
                (name.clone(), sig)
            }
            ExprKind::MethodCall {
                receiver, method, ..
            } => {
                let sig = find_method_sig(receiver, method, &ctx);
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
                label: ParameterLabel::Simple(format!(
                    "{}: {}",
                    p.name,
                    format_resolved_type(&p.ty, registry)
                )),
                documentation: None,
            })
            .collect();

        let params_str: Vec<String> = sig
            .params
            .iter()
            .filter(|p| p.name != "self")
            .map(|p| format!("{}: {}", p.name, format_resolved_type(&p.ty, registry)))
            .collect();
        let label = format!(
            "fn {}({}) -> {}",
            function_name,
            params_str.join(", "),
            format_resolved_type(&sig.return_type, registry)
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

fn find_function_sig<'a>(
    name: &str,
    package: &str,
    registry: &'a GlobalRegistry,
) -> Option<&'a FunctionSignature> {
    for pkg in [package, "Global"] {
        let ident = Identifier::new(pkg, vec![name.to_string()]);
        if let Some((_, entry)) = registry.lookup(&ident)
            && let GlobalKind::Function(Some(sig)) = &entry.kind
        {
            return Some(sig);
        }
    }
    None
}

fn find_method_sig<'a>(
    receiver: &expo_ast::ast::Expr,
    method: &str,
    ctx: &LookupCtx<'a>,
) -> Option<&'a FunctionSignature> {
    let type_id = traverse_receiver_type_id(receiver, ctx)?;
    let type_entry = ctx.registry.get(type_id)?;
    let pkg = type_entry.identifier.package();
    let type_name = type_entry.identifier.last();
    let method_ident = Identifier::new(pkg, vec![type_name.to_string(), method.to_string()]);
    let (_, method_entry) = ctx.registry.lookup(&method_ident)?;
    match &method_entry.kind {
        GlobalKind::Function(Some(sig)) => Some(sig),
        _ => None,
    }
}
