//! Signature help provider for the Koja LSP.
//!
//! When the cursor is inside a function or method call's argument list,
//! displays the parameter names and types with the active parameter
//! highlighted. Supports both free functions and method calls.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::ExprKind;
use koja_ast::identifier::Resolution;
use koja_typecheck::{FunctionSignature, GlobalKind, GlobalRegistry};

use crate::backend::Backend;
use crate::format::format_resolved_type;
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
        let (file, registry) = match (state.active_file(), state.registry()) {
            (Some(f), Some(r)) => (f, r),
            _ => return Ok(None),
        };

        let line = pos.line + 1;
        let col = pos.character + 1;
        let call_site = match find_enclosing_call(file, line, col) {
            Some(c) => c,
            None => return Ok(None),
        };

        let (function_name, sig) = match &call_site.expr.kind {
            ExprKind::Call { callee, .. } => {
                let ExprKind::Ident { name, resolution } = &callee.kind else {
                    return Ok(None);
                };
                let sig = function_signature_for_target(*resolution, registry);
                (name.clone(), sig)
            }
            ExprKind::MethodCall { method, target, .. } => {
                let sig = function_signature_for_target(*target, registry);
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

fn function_signature_for_target(
    target: Resolution,
    registry: &GlobalRegistry,
) -> Option<&FunctionSignature> {
    let Resolution::Global(id) = target else {
        return None;
    };
    match &registry.get(id)?.kind {
        GlobalKind::Function(definition) => definition.signature.as_ref(),
        _ => None,
    }
}
