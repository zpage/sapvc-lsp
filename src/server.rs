use std::collections::HashMap;
use std::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{async_trait, Client, LanguageServer};

use crate::analyze;
use crate::material::MaterialIndex;
use crate::refs::ReferenceIndex;

#[derive(Debug, Default)]
pub struct Documents {
    map: Mutex<HashMap<Url, String>>,
}

impl Documents {
    pub fn get(&self, uri: &Url) -> Option<String> {
        self.map.lock().unwrap().get(uri).cloned()
    }
    pub fn insert(&self, uri: Url, text: String) {
        self.map.lock().unwrap().insert(uri, text);
    }
    pub fn remove(&self, uri: &Url) {
        self.map.lock().unwrap().remove(uri);
    }
}

pub struct Backend {
    client: Client,
    docs: Documents,
    material: MaterialIndex,
    refs: ReferenceIndex,
    refs_dir: Option<std::path::PathBuf>,
}

impl Backend {
    pub fn new(client: Client, data_file: Option<&std::path::Path>) -> Self {
        let material = MaterialIndex::load(data_file);
        // where-used index: scan material-data/<mat>/ alongside the package
        let mut refs_dir: Option<std::path::PathBuf> = None;
        let refs = data_file
            .and_then(|p| p.parent())
            .map(|parent| parent.join("material-data"))
            .filter(|md| md.is_dir())
            .and_then(|md| {
                let name = material
                    .material
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let dir = md.join(&name);
                if dir.is_dir() {
                    refs_dir = Some(dir.clone());
                    Some(ReferenceIndex::scan(&dir))
                } else {
                    None
                }
            })
            .unwrap_or_default();
        eprintln!(
            "[sapvc-lsp] reference index: {} identifiers across material corpus",
            refs.refs.len()
        );
        Self {
            client,
            docs: Documents::default(),
            material,
            refs,
            refs_dir,
        }
    }

    /// Publish diagnostics for one open document (syntax + semantic).
    async fn publish_diagnostics(&self, uri: &Url) {
        let Some(text) = self.docs.get(uri) else { return };

        let mut parser = tree_sitter::Parser::new();
        if parser
            .set_language(&tree_sitter_sapvc::language())
            .is_err()
        {
            return;
        }
        let tree = match parser.parse(&text, None) {
            Some(t) => t,
            None => return,
        };

        let mut all = analyze::diagnostics(&text);
        all.extend(analyze::semantic_diagnostics(&text, &tree, &self.material));
        self.client
            .publish_diagnostics(uri.clone(), all, None)
            .await;
    }
}

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        // nothing to do yet
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        self.docs.insert(uri.clone(), params.text_document.text);
        self.publish_diagnostics(&uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // Full sync: the change array holds the complete new text.
        if let Some(change) = params.content_changes.last() {
            self.docs.insert(uri.clone(), change.text.clone());
        }
        self.publish_diagnostics(&uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.remove(&params.text_document.uri);
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some(text) = self.docs.get(uri) else {
            return Ok(None);
        };
        Ok(crate::completion::complete(&text, position, &self.material))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some(text) = self.docs.get(uri) else {
            return Ok(None);
        };
        // word under cursor
        let Some(offset) = crate::hover::position_to_byte(&text, position) else {
            return Ok(None);
        };
        let before = &text[..offset];
        let word: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if word.is_empty() {
            return Ok(None);
        }
        let base_uri = uri.clone();
        let mut locations: Vec<Location> = vec![];
        // cross-file references from the material corpus index
        if let Some(locs) = self.refs.refs.get(&word) {
            for l in locs {
                let file_uri = Url::from_file_path(
                    self.refs_dir
                        .as_ref()
                        .map(|d| d.join(&l.file))
                        .unwrap_or_else(|| std::path::PathBuf::from(&l.file)),
                )
                .unwrap_or_else(|_| base_uri.clone());
                let line = (l.line.saturating_sub(1)) as u32;
                locations.push(Location {
                    uri: file_uri,
                    range: Range {
                        start: Position { line, character: 0 },
                        end: Position { line, character: 0 },
                    },
                });
            }
        }
        // current document occurrences (line-local dotted tails)
        for (i, line) in text.lines().enumerate() {
            if line.contains(&word) {
                locations.push(Location {
                    uri: base_uri.clone(),
                    range: Range {
                        start: Position { line: i as u32, character: 0 },
                        end: Position { line: i as u32, character: 0 },
                    },
                });
            }
        }
        locations.sort_by_key(|l| (l.uri.to_string(), l.range.start.line));
        locations.dedup_by(|a, b| a.uri == b.uri && a.range.start.line == b.range.start.line);
        Ok(Some(locations))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some(text) = self.docs.get(uri) else {
            return Ok(None);
        };
        let current_file = uri
            .path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or("");
        Ok(crate::hover::hover(&text, position, &self.material, &self.refs, current_file))
    }
}
