//! A tiny language server that makes SFCC paths clickable.
//!
//! It answers `textDocument/definition` only, for three things an SFCC editor
//! cannot follow on its own:
//!   * `template="..."` on `<isinclude>` / `<isdecorate>` / `<ismodule>`
//!   * `require('*/cartridge/...')`, `~/`, relative and cartridge-qualified
//!   * `Resource.msg('key', 'bundle')` / `msgf` / `i18nMessage`
//!
//! Everything else is left to the language servers Zed already runs.

mod reference;
mod resolve;
mod workspace;

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
};
use lsp_types::request::{GotoDefinition, Request as RequestTrait};
use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, InitializeParams, Location, OneOf, Position,
    Range, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

use crate::workspace::Workspace;

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        definition_provider: Some(OneOf::Left(true)),
        ..Default::default()
    })?;

    let initialize_params = connection.initialize(capabilities)?;
    let server = Server::new(serde_json::from_value(initialize_params)?);
    server.run(&connection)?;

    io_threads.join()?;
    Ok(())
}

struct Server {
    workspace: Workspace,
    documents: HashMap<Url, String>,
}

impl Server {
    fn new(params: InitializeParams) -> Self {
        Server {
            workspace: Workspace::scan(&workspace_roots(&params)),
            documents: HashMap::new(),
        }
    }

    fn run(mut self, connection: &Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
        for message in &connection.receiver {
            match message {
                Message::Request(request) => {
                    if connection.handle_shutdown(&request)? {
                        return Ok(());
                    }
                    let response = self.respond(request);
                    connection.sender.send(Message::Response(response))?;
                }
                Message::Notification(notification) => self.apply(notification),
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn respond(&self, request: Request) -> Response {
        let id = request.id.clone();
        if request.method != GotoDefinition::METHOD {
            return Response::new_ok(id, serde_json::Value::Null);
        }
        match cast::<GotoDefinition>(request) {
            Ok((id, params)) => Response::new_ok(id, self.definition(params)),
            Err(_) => Response::new_ok(id, serde_json::Value::Null),
        }
    }

    fn definition(&self, params: GotoDefinitionParams) -> Option<GotoDefinitionResponse> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let text = self.documents.get(&uri)?;
        let file = uri.to_file_path().ok()?;

        let line = text.lines().nth(position.position.line as usize)?;
        let column = char_offset(line, position.position.character as usize);
        let reference = reference::at_cursor(line, column)?;

        let locations: Vec<Location> = resolve::resolve(&reference, &file, &self.workspace)
            .into_iter()
            .filter_map(|hit| {
                let target = Url::from_file_path(&hit.path).ok()?;
                let start = Position::new(hit.line, 0);
                Some(Location::new(target, Range::new(start, start)))
            })
            .collect();

        (!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations))
    }

    fn apply(&mut self, notification: lsp_server::Notification) {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(
                    notification.params,
                ) {
                    self.documents
                        .insert(params.text_document.uri, params.text_document.text);
                }
            }
            DidChangeTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(
                    notification.params,
                ) {
                    if let Some(change) = params.content_changes.into_iter().next_back() {
                        self.documents.insert(params.text_document.uri, change.text);
                    }
                }
            }
            DidCloseTextDocument::METHOD => {
                if let Ok(params) = serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(
                    notification.params,
                ) {
                    self.documents.remove(&params.text_document.uri);
                }
            }
            _ => {}
        }
    }
}

fn workspace_roots(params: &InitializeParams) -> Vec<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        let roots: Vec<PathBuf> = folders
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().ok())
            .collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    #[allow(deprecated)]
    params
        .root_uri
        .as_ref()
        .and_then(|uri| uri.to_file_path().ok())
        .into_iter()
        .collect()
}

/// LSP columns are UTF-16 code units; the reference scanner works in chars.
fn char_offset(line: &str, utf16_column: usize) -> usize {
    let mut utf16 = 0;
    for (index, c) in line.chars().enumerate() {
        if utf16 >= utf16_column {
            return index;
        }
        utf16 += c.len_utf16();
    }
    line.chars().count()
}

fn cast<R>(request: Request) -> Result<(RequestId, R::Params), ExtractError<Request>>
where
    R: RequestTrait,
    R::Params: serde::de::DeserializeOwned,
{
    request.extract(R::METHOD)
}

#[cfg(test)]
mod tests {
    use super::char_offset;

    #[test]
    fn maps_utf16_columns_to_char_indices() {
        assert_eq!(char_offset("abc", 2), 2);
        // An emoji is two UTF-16 units but one char.
        assert_eq!(char_offset("💾ab", 3), 2);
    }
}
