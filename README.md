# sfcc-tools

Editor and command-line tooling for Salesforce B2C Commerce (SFCC), in Rust.

An SFCC checkout gives a general-purpose editor nothing to work with: no type definitions
for the `dw.*` API, no way to resolve a cartridge-relative `require`, no record of which of
the four cartridges declaring a route is the one that runs. And the loop between saving a
file and seeing it on a sandbox goes through a separate uploader that the editor cannot
see. These are the pieces that close those gaps.

| | What it is |
| --- | --- |
| [`crates/prost`](crates/prost) | Cartridge uploader for sandboxes, as a CLI. Same job as the Prophet VS Code extension, with no editor attached |
| [`crates/isml-lsp`](crates/isml-lsp) | Language server: ISML and `dw.*` completion, custom-attribute checks, route override chains, go-to-definition on SFCC paths |
| [`grammar`](grammar) | `tree-sitter-isml` — the only tree-sitter grammar for ISML there is |
| [`extensions/isml`](extensions/isml) | Zed extension wiring the grammar and the language server together |
| [`extensions/b2c-debug`](extensions/b2c-debug) | Zed extension registering a debug adapter for server-side scripts |

Each has its own README; this one only says how they fit together.

## Layout

```
crates/            the two Rust programs, one cargo workspace
grammar/           tree-sitter-isml, buildable on its own
extensions/        the Zed extensions, deliberately outside the workspace
```

The extensions sit outside the workspace because Zed compiles an extension in place with a
`--target-dir` it hardcodes inside the extension's own directory; a crate belonging to a
workspace resolves against the workspace root instead.

## Building

```bash
cargo test                     # both crates
cargo build --release          # both binaries
cargo doc --no-deps --open     # the language server's reference
```

## History

This repository is the three that came before it, merged with their history intact:
[`prost`](https://github.com/salva-sm/prost),
[`sfcc-zed-isml`](https://github.com/salva-sm/sfcc-zed-isml) and
[`sfcc-zed-debugger`](https://github.com/salva-sm/sfcc-zed-debugger). They are archived;
everything continues here.

## Licence

MIT.
