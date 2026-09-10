# sfcc-zed-isml

ISML support for the [Zed](https://zed.dev) editor: syntax highlighting for
Salesforce B2C Commerce templates, and ctrl/cmd-click navigation on the paths that
appear in them.

Three pieces, each buildable on its own:

- **`grammar/`** — `tree-sitter-isml`, a fork of `tree-sitter-html` that understands ISML
  tags, `${ ... }` expressions and `<isscript>` / `<iscomment>` raw text. The only
  tree-sitter grammar for ISML there is.
- **`extension/`** — the Zed extension: language registration, highlight/injection
  queries, and the glue that launches the language server.
- **`isml-lsp/`** — a small language server that answers `textDocument/definition` for
  SFCC paths. Zed has no built-in way to make a path in a string clickable, so this is
  what turns `require('*/cartridge/...')` into a jump.

## Install

The extension is not in the Zed registry yet. Two ways in:

**From a release zip** — nothing to build, no toolchain:

1. Download `isml-<version>.zip` from [Releases](https://github.com/salva-sm/sfcc-zed-isml/releases).
2. Unzip it anywhere and run `install.ps1`.

**From source** — needs Rust:

```powershell
.\install.ps1                 # builds isml-lsp onto your PATH
```

Then, from Zed's command palette: **`zed: install dev extension`** and pick the
`extension` folder (**`zed: reload extensions`** if it is already installed).

The extension looks for `isml-lsp` on `PATH` first, then falls back to downloading the
matching binary from this repo's releases, so it works either way.

## What becomes clickable

Ctrl/cmd-click (or `Go to Definition`) on:

| In the code | Jumps to |
| ----------- | -------- |
| `<isinclude template="account/dashboard"/>` | `<cartridge>/cartridge/templates/default/account/dashboard.isml` |
| `<isdecorate template="...">`, `<ismodule template="...">` | same |
| `require('*/cartridge/scripts/x')` | that file in **every** cartridge that has it |
| `require('~/cartridge/scripts/x')` | the current cartridge only |
| `require('./sibling')`, `require('../x')` | relative to the file |
| `require('app_common_eu_guess/cartridge/...')` | that cartridge |
| `require('server')` and other bare names | `cartridges/modules/<name>` |
| `Resource.msg('key', 'bundle')`, `msgf`, `i18nMessage` | the `.properties` line defining the key |

Cartridge overrides are all returned, ordered with the current file's cartridge first, so
Zed shows the override chain in a picker instead of guessing one. Resource keys resolve to
the exact line; if no bundle defines the key, the candidate bundles are offered instead.

The server is registered for **ISML and JavaScript**, because `require('*/cartridge/...')`
is just as unnavigable in a controller as in a template. It only ever answers when the
cursor is on one of the paths above, so it never competes with the TypeScript server. To
turn it off for JavaScript, remove `"JavaScript"` from `languages` in
`extension/extension.toml`, or disable the `isml-lsp` server in your Zed settings.

## Highlighting

The grammar parses 98.9% of the 2185 ISML templates in `sfcc-eu` without a single error
node; the remainder are templates with genuinely unbalanced markup (a stray `</div>`,
`</tr class="...">`, a dynamic `<${expr}>` tag name).

On top of plain HTML it handles the ISML idioms that break an HTML parser:

```isml
<isset name="x" value="${a > b}"/>                   ${} swallows the > 
<input ${cond ? 'checked' : ''} />                   bare expression as an attribute
<form <isprint value="${form.attributes}"/>>         ISML tag in the attribute list
<div class="a <isif condition="${x}">on</isif>">     ISML tag inside an attribute value
<meta name="<isprint value="${tag.ID}">">            void ISML tag closing at the quote
<iscomment><isif ...>not parsed</isif></iscomment>   raw text
```

JavaScript is injected into `${ ... }`, `<isscript>` and `<script>`; CSS into `<style>`.
ISML tags get the keyword colour, HTML tags the tag colour.

## Sharing it with the team

Zed has no "install from file" command, but it *watches*
`%LOCALAPPDATA%\Zed\extensions\installed` and re-indexes whatever appears there, so a
prebuilt folder is all a teammate needs — no Rust, no cargo, no tree-sitter, no clang.

**CI builds that zip on every push**, in `extension-zip.yml`, and attaches it to the release
on a tag — so normally there is nothing to do by hand. It reproduces what Zed's own builder
does: `cargo build --release --target wasm32-wasip2` for `extension.wasm`, and the wasi-sdk
clang with Zed's flags for `grammars/isml.wasm`. Zed additionally strips custom sections from
the wasm, which is a size optimisation; the `zed:api-version` section it reads at load time
survives either way. The job asserts the result is complete before uploading it, so a green
run means an installable zip.

To build one locally instead — the only way to include the language server binary, which the
CI zip leaves out because the extension downloads it:

```powershell
.\package.ps1 -Binary C:\rust\cargo\bin\isml-lsp.exe   # -> dist\isml-0.1.0.zip
.\package.ps1 -ExtensionDir C:\dev\zed-b2c-debug       # any other extension
```

Either way the payload is only what Zed loads at runtime — `extension.toml`,
`extension.wasm`, `grammars\*.wasm`, `languages\` — plus `packaging/install.ps1`, which both
routes copy verbatim so the two zips cannot drift apart. `package.ps1` needs an
`extension.wasm`, and Zed writes that when you run `zed: install dev extension`, so install
the extension here at least once before packaging locally.

Note that installing the package on this machine replaces the dev-extension symlink with
a static copy; re-run `zed: install dev extension` to go back to developing.

## Releasing

`isml-lsp` binaries are what a user without Rust gets, so a release is a tag:

```powershell
.\install.ps1 -PinGrammar    # only if grammar\ changed since the last release
# commit the manifest, then
git tag v0.1.0 ; git push --tags
```

`release.yml` builds `isml-lsp` for Windows, macOS and Linux (x86_64 and aarch64) and
attaches the archives with the names `download_binary` in `extension/src/isml.rs`
expects. Keep those two in sync.

Note the chicken-and-egg in `[grammars.isml]`: the grammar lives in this repo, so the
pinned commit is always an earlier one. That is fine — it only has to be a commit whose
`grammar/` is the version you want.

## Publishing to the Zed registry

The [prerequisites](https://zed.dev/docs/extensions/publishing/prerequisites) are met:
public repo, MIT licence, kebab-case id without "zed"/"extension", a grammar declared in
the manifest, and a language server that is downloaded rather than bundled.

The submission is a PR to `zed-industries/extensions` adding this repo as a
submodule plus an entry in `extensions.toml`:

```toml
[isml]
submodule = "extensions/isml"
path = "extension"
version = "0.1.0"
```

## Development

```bash
cd grammar
npx tree-sitter generate                                     # after editing grammar.js
bash check.sh                                                # fixtures + query load
bash check.sh ~/Github/sfcc-eu/source/cartridges             # ...and a real corpus
CC=/c/rust/zig/clang.cmd npx tree-sitter parse some.isml     # inspect one parse tree

cd ../isml-lsp
cargo test
node smoke-test.mjs <workspace-root> <file.isml> "<needle>"  # end-to-end over stdio
```

### Why this lives in `C:\dev` and not in `~\Github`

**It has to.** Zed compiles an extension in place and hardcodes the target directory:

```rust
// crates/extension/src/extension_builder.rs
.arg("--target-dir").arg(extension_dir.join("target"))
```

The Windows profile on this machine is `C:\Users\SalvadorSánchezMénde`, and the MinGW
linker cannot resolve a path containing `á`/`é`. Under `~\Github` the host proc-macros
(`serde_derive`, `zerofrom_derive`) die with `ld: cannot find ...` for every object file
and Zed reports only *"failed to compile Rust extension"* — the real error is in
`%LOCALAPPDATA%\Zed\logs\Zed.log`. A `.cargo/config.toml` cannot help (Zed's
`--target-dir` wins) and neither can a junction (cargo canonicalises through it).

Two smaller local details, both about the C compiler:

- There is no MSVC and no full MinGW here, so the tree-sitter CLI builds the parser with
  `zig cc`, wrapped by `C:\rust\zig\clang.cmd` — which drops the
  `--target=x86_64-pc-windows-msvc` the CLI passes and zig rejects. `check.sh` sets `CC`
  for you. Zed itself uses its own downloaded wasi-sdk, so none of this is needed just to
  *use* the extension.
- `strip = true` must stay out of `isml-lsp`'s release profile: on `windows-gnu` it strips
  the metadata out of proc-macro DLLs and the build fails with
  `found staticlib ... instead of rlib`.

## License

MIT. The grammar keeps `tree-sitter-html`'s copyright notice in `grammar/LICENSE`.
