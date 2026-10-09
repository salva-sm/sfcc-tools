# LemMinX for Zed

XML language support for the [Zed](https://zed.dev) editor through
[Eclipse LemMinX](https://github.com/eclipse-lemminx/lemminx), the server behind Red Hat's
XML extension for VS Code: completion, validation, hover and document symbols from XSD and
DTD, XML catalogs and file associations.

Zed's **XML** extension gives the language and its highlighting; this one adds the server.
Install both.

## Install

The extension is not in the Zed registry yet. From Zed's command palette:
**`zed: install dev extension`** and pick this `extensions/lemminx` folder
(**`zed: reload extensions`** if it is already installed). Building it needs Rust with the
`wasm32-wasip2` target.

The server binary is found in this order:

1. `lsp.lemminx.binary.path` in your settings;
2. `lemminx` on `PATH`;
3. otherwise it is downloaded: the native binary Red Hat publishes with each release of
   [`vscode-xml`](https://github.com/redhat-developer/vscode-xml/releases), for Windows
   x86-64, macOS and Linux (x86-64 and ARM). No Java needed. On any other platform, point
   `binary.path` at a LemMinX build.

## Settings

Everything under `lsp.lemminx.settings` reaches LemMinX as its `xml` settings, in the
initialization options and on every `workspace/configuration` request. The full list is in
[LemMinX's documentation](https://github.com/eclipse-lemminx/lemminx/blob/main/docs/Configuration.md).

Relative paths in `xml.catalogs` and in `xml.fileAssociations[].systemId` are resolved
against the project root, as the VS Code client does, so they can live in a project's
`.zed/settings.json`:

```jsonc
{
    "lsp": {
        "lemminx": {
            "settings": {
                "xml": {
                    // Maps namespaces to schemas: completion in any document of a known namespace.
                    "catalogs": ["schemas/catalog.xml"],
                    // Binds files to a schema: what turns on validation for documents that
                    // carry no xsi:schemaLocation of their own.
                    "fileAssociations": [
                        { "pattern": "**/config/*.xml", "systemId": "schemas/config.xsd" }
                    ]
                }
            }
        }
    }
}
```

A catalog is enough for completion, but LemMinX only **validates** a document bound to a
grammar - by `xsi:schemaLocation`, a DOCTYPE or a file association. A document that only
declares its namespace gets completion from the catalog and no validation.

Absolute paths and URIs (`https://…`, `file:///…`) are passed through untouched.
