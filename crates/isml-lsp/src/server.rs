//! The language server itself: the request loop and every handler.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    PublishDiagnostics,
};
use lsp_types::request::{
    Completion, GotoDefinition, HoverRequest, Request as RequestTrait, ResolveCompletionItem,
};
use lsp_types::{
    CompletionItem, CompletionList, CompletionOptions, CompletionParams, CompletionResponse,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeParams, Location, MarkupContent, MarkupKind, OneOf,
    Position, PublishDiagnosticsParams, Range, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url,
};

use crate::complete::{self, Completer};
use crate::metadata::Metadata;
use crate::workspace::Workspace;
use crate::{diagnose, hover, reference, resolve, sync, validate};

/// Serve one editor session over stdio, until it disconnects.
pub fn serve() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        definition_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            // `<` opens a tag, `.` reaches a custom attribute, a quote opens an
            // attribute value or the argument of getCustomPreferenceValue.
            trigger_characters: Some(["<", " ", ".", "\"", "'"].map(str::to_string).to_vec()),
            // An editor is entitled to ignore the `additionalTextEdits` sent
            // with an item and ask for them here instead, and several do —
            // without this, the `require` an import completion writes never
            // reaches the document.
            resolve_provider: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    })?;

    let initialize_params = connection.initialize(capabilities)?;
    let params: InitializeParams = serde_json::from_value(initialize_params)?;
    sync::report(workspace_roots(&params), connection.sender.clone());

    let server = Server::new(params);
    server.run(&connection)?;

    // The writer thread ends when the last sender goes, and the connection
    // holds one: without this, join() blocks and the process outlives Zed.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// One editor session: the open documents, and the indexes answers come from.
pub struct Server {
    workspace: Workspace,
    metadata: Metadata,
    documents: HashMap<Url, String>,
}

impl Server {
    fn new(params: InitializeParams) -> Self {
        let roots = workspace_roots(&params);
        Server {
            workspace: Workspace::scan(&roots),
            metadata: Metadata::scan(&roots),
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
                Message::Notification(notification) => {
                    if let Some(uri) = self.apply(notification) {
                        connection.sender.send(self.published(uri))?;
                    }
                }
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn respond(&self, request: Request) -> Response {
        let id = request.id.clone();
        let answer = match request.method.as_str() {
            GotoDefinition::METHOD => cast::<GotoDefinition>(request)
                .ok()
                .and_then(|(_, params)| serde_json::to_value(self.definition(params)?).ok()),
            Completion::METHOD => cast::<Completion>(request)
                .ok()
                .and_then(|(_, params)| serde_json::to_value(self.completion(params)?).ok()),
            HoverRequest::METHOD => cast::<HoverRequest>(request)
                .ok()
                .and_then(|(_, params)| serde_json::to_value(self.hover(params)?).ok()),
            ResolveCompletionItem::METHOD => cast::<ResolveCompletionItem>(request)
                .ok()
                .and_then(|(_, item)| serde_json::to_value(resolve(item)).ok()),
            _ => None,
        };
        Response::new_ok(id, answer.unwrap_or(serde_json::Value::Null))
    }

    fn completion(&self, params: CompletionParams) -> Option<CompletionResponse> {
        let position = params.text_document_position;
        let uri = position.text_document.uri;
        let text = self.documents.get(&uri)?;
        let offset = char_offset_at(text, position.position)?;
        let file = uri.to_file_path().ok()?;

        let context = complete::context_at(text, offset, is_isml(&uri))?;
        let completer = Completer {
            workspace: &self.workspace,
            metadata: &self.metadata,
            text,
            file: &file,
        };
        let items = completer.items(&context, position.position);
        (!items.is_empty()).then_some(CompletionResponse::List(CompletionList {
            is_incomplete: false,
            items,
        }))
    }

    fn hover(&self, params: HoverParams) -> Option<Hover> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let file = uri.to_file_path().ok()?;
        let text = self.documents.get(&uri)?;
        let line = text.lines().nth(position.position.line as usize)?;
        let column = char_offset(line, position.position.character as usize);

        let value = hover::member_markdown(line, column, text)
            .or_else(|| self.reference_hover(&uri, position.position, &file))?;
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: None,
        })
    }

    /// What a reference under the cursor is worth saying: the override chain
    /// of a route, or what an API class is for.
    fn reference_hover(&self, uri: &Url, position: Position, file: &Path) -> Option<String> {
        let reference = self.reference_at(uri, position)?;
        if let Some(text) = hover::module_markdown(&reference) {
            return Some(text);
        }
        let route = hover::route_of(&reference, file)?;
        let chains = hover::chains(&route, &self.workspace);
        hover::markdown(&route, &chains, file)
    }

    /// The SFCC reference under a position, shared by hover and definition.
    fn reference_at(&self, uri: &Url, position: Position) -> Option<reference::Reference> {
        let text = self.documents.get(uri)?;
        let line = text.lines().nth(position.line as usize)?;
        reference::at_cursor(line, char_offset(line, position.character as usize))
    }

    fn published(&self, uri: Url) -> Message {
        let diagnostics = self
            .documents
            .get(&uri)
            .map(|text| self.check(&uri, text))
            .unwrap_or_default();
        let params = PublishDiagnosticsParams {
            uri,
            diagnostics,
            version: None,
        };
        Message::Notification(lsp_server::Notification::new(
            PublishDiagnostics::METHOD.to_string(),
            params,
        ))
    }

    /// Each check answers for the file types it recognises and stays quiet
    /// for the rest, so a document only ever gets the one that applies.
    fn check(&self, uri: &Url, text: &str) -> Vec<lsp_types::Diagnostic> {
        // `.custom.` is script, so a JSON description that happens to mention
        // one is prose, not an access.
        let path = uri.path();
        let mut found = match path.ends_with(".js") || path.ends_with(".isml") {
            true => diagnose::diagnostics(text, &self.metadata),
            false => Vec::new(),
        };
        if let Ok(file) = uri.to_file_path() {
            found.extend(validate::diagnostics(&file, text, &self.workspace));
        }
        found
    }

    fn definition(&self, params: GotoDefinitionParams) -> Option<GotoDefinitionResponse> {
        let position = params.text_document_position_params;
        let uri = position.text_document.uri;
        let file = uri.to_file_path().ok()?;
        let reference = self.reference_at(&uri, position.position)?;

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

    /// The document whose diagnostics are now stale, if the notification
    /// changed one.
    fn apply(&mut self, notification: lsp_server::Notification) -> Option<Url> {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params: lsp_types::DidOpenTextDocumentParams =
                    serde_json::from_value(notification.params).ok()?;
                let uri = params.text_document.uri;
                self.documents
                    .insert(uri.clone(), params.text_document.text);
                Some(uri)
            }
            DidChangeTextDocument::METHOD => {
                let params: lsp_types::DidChangeTextDocumentParams =
                    serde_json::from_value(notification.params).ok()?;
                let change = params.content_changes.into_iter().next_back()?;
                let uri = params.text_document.uri;
                self.documents.insert(uri.clone(), change.text);
                Some(uri)
            }
            DidCloseTextDocument::METHOD => {
                let params: lsp_types::DidCloseTextDocumentParams =
                    serde_json::from_value(notification.params).ok()?;
                self.documents.remove(&params.text_document.uri);
                // Published empty, so what the file used to report disappears.
                Some(params.text_document.uri)
            }
            _ => None,
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

/// Attach the `require` line an import completion promised. An editor may
/// ignore the edits sent with the item and ask for them here instead, so the
/// item carries what it needs to rebuild them.
fn resolve(mut item: CompletionItem) -> CompletionItem {
    let Some(data) = item.data.take() else {
        return item;
    };
    if let Ok(pending) = serde_json::from_value::<complete::PendingRequire>(data) {
        item.additional_text_edits = Some(vec![pending.edit()]);
    }
    item
}

/// Character offset of a line/column position into the whole document.
/// Split on `\n` rather than `lines()`, so a CRLF file does not drift by one
/// character per line.
fn char_offset_at(text: &str, position: Position) -> Option<usize> {
    let mut offset = 0;
    for (number, line) in text.split('\n').enumerate() {
        if number == position.line as usize {
            return Some(offset + char_offset(line, position.character as usize));
        }
        offset += line.chars().count() + 1;
    }
    None
}

/// The tag completions only make sense in a template; the custom-attribute
/// ones apply to JavaScript as well.
fn is_isml(uri: &Url) -> bool {
    uri.path().ends_with(".isml")
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
