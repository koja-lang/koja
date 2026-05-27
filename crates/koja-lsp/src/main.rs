mod backend;
mod completion;
mod convert;
mod definition;
mod diagnostics;
mod folding;
mod format;
mod hover;
mod lookup;
mod signature_help;
mod symbols;

use tower_lsp_server::{LspService, Server};

#[tokio::main]
async fn main() {
    eprintln!("[koja-lsp] starting server");

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(backend::Backend::new);
    eprintln!("[koja-lsp] serving on stdio");
    Server::new(stdin, stdout, socket).serve(service).await;

    eprintln!("[koja-lsp] server exited");
}
