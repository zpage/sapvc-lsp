use tower_lsp::{LspService, Server};

mod analyze;
mod completion;
mod hover;
mod material;
mod refs;
mod server;

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // --data <path>: material data package (export_material.py output) for
    // semantic diagnostics and profile-scoped completion.
    let mut data_file: Option<std::path::PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--data" {
            data_file = args.next().map(std::path::PathBuf::from);
        }
    }

    let (service, socket) = LspService::new(|client| server::Backend::new(client, data_file.as_deref()));
    Server::new(stdin, stdout, socket).serve(service).await;
}
