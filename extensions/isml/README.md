# ISML for Zed

SFCC support for the [Zed](https://zed.dev) editor: syntax highlighting for ISML
templates, completion for the ISML tag set, the `dw.*` script API and the custom
attributes your own metadata defines, the override chain of every SFRA route, checks on
the configuration files nothing else checks, and ctrl/cmd-click navigation on the paths
that appear in all of it.

Three pieces, each buildable on its own:

- **`../../grammar/`** — `tree-sitter-isml`, a fork of `tree-sitter-html` that understands ISML
  tags, `${ ... }` expressions and `<isscript>` / `<iscomment>` raw text. The only
  tree-sitter grammar for ISML there is.
- **this directory** — the Zed extension: language registration, highlight/injection
  queries, and the glue that launches the language server.
- **`../../crates/isml-lsp/`** — a library plus the thin binary that serves it. It knows
  what no general editor can: SFCC paths, the ISML tag set, the `dw.*` API, the object
  metadata checked into the repository, and the cartridge path that decides which
  `server.append` runs. It answers `textDocument/definition`, `textDocument/completion`,
  `textDocument/hover` and `textDocument/publishDiagnostics`.

## Install

The extension is not in the Zed registry yet. Two ways in:

**From a release zip** — nothing to build, no toolchain:

1. Download `isml-<version>.zip` from [Releases](https://github.com/salva-sm/sfcc-tools/releases).
2. Unzip it anywhere and run `install.cmd`. (Windows blocks a downloaded `.ps1`
   under the default execution policy; the `.cmd` next to it is the way past that.)

**From source** — needs Rust:

```powershell
# from the repository root
.\tools\install-lsp.ps1    # builds isml-lsp onto your PATH
```

Then, from Zed's command palette: **`zed: install dev extension`** and pick this
`extensions\isml` folder (**`zed: reload extensions`** if it is already installed).

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
| `require('app_brand/cartridge/...')` | that cartridge |
| `require('server')` and other bare names | `cartridges/modules/<name>` |
| `Resource.msg('key', 'bundle')`, `msgf`, `i18nMessage` | the `.properties` line defining the key |

Cartridge overrides are all returned, ordered with the current file's cartridge first, so
Zed shows the override chain in a picker instead of guessing one. Resource keys resolve to
the exact line; if no bundle defines the key, the candidate bundles are offered instead.

The server is registered for **ISML and JavaScript**, because `require('*/cartridge/...')`
is just as unnavigable in a controller as in a template. It only ever answers when the
cursor is on one of the paths above, so it never competes with the TypeScript server. To
turn it off for JavaScript, remove `"JavaScript"` from `languages` in
`extensions/isml/extension.toml`, or disable the `isml-lsp` server in your Zed settings.

## What it completes

Zed falls back to HTML in a template, which offers `<is:include>` and other tags that do
not exist. The server answers with the real set instead, so the wrong one stops winning.

| Where the cursor is | What is offered |
| ------------------- | --------------- |
| `<is` | the 32 ISML tags, each inserting a closed skeleton: `isif` becomes `<isif condition="">…</isif>` |
| `<isloop ` | that tag's attributes, the required ones first |
| `<isset ... scope="` | the values the platform accepts — `page`, `request`, `session` |
| `<isinclude template="` | every template path in the workspace, without the locale folder or the `.isml` |
| `product.custom.` | the custom attributes the metadata defines for `Product`, with type, enumerated values and display name |
| `getCustomPreferenceValue('` | the site preferences the metadata defines |
| `server.append('` | the routes this controller already has somewhere in the cartridge path |
| `require('` | the 398 `dw/...` modules, each with what the class is for |
| `Site.` — any name bound by `require('dw/...')` | that class's methods, properties and constants, with signatures |
| `dwSite` | inserts `Site`, and writes `var Site = require('dw/system/Site');` into the require block |
| `var Site` | completes the declaration in place, `require` and all |

Everything from `product.custom.` down works in a `.js` controller as well as in a
template, and inside `${ ... }`.

### Importing what you name

Having the API is only half of it if bringing a class in is still manual. Two ways,
whichever suits where the cursor already is:

```js
    dwTransaction          ->  Transaction
                               ...and `var Transaction = require('dw/system/Transaction');`
                               appears in the require block above

var Transaction            ->  var Transaction = require('dw/system/Transaction');
```

The new line goes under the last existing `require`, or under `'use strict'` when there
is none, and it borrows whichever of `var`, `const` or `let` the file already uses. A
class the document already requires is inserted under **its existing name**, with no
second copy of the line — and the completion says so.

`var dwTransaction` works too: inside a declaration the `dw` prefix is optional.

## What it flags

An attribute that the metadata does not define is a warning where it is written:

```
product.custom.seasson
               ~~~~~~~  `seasson` is not a custom attribute of `Product` in the metadata.
```

SFCC returns `undefined` rather than failing, so a typo like that otherwise costs a deploy
and a page load to find.

### Where the metadata comes from

Any `*objecttype-extensions.xml` or `*objecttype-definitions.xml` in the open folder — the
same files a site import uses — often under a `metadata/` directory at the repository
root. Reading a few thousand attribute definitions takes a fraction of a second at
startup. **With no such file in the folder, completion returns nothing and no diagnostic
is ever published**: an unknown
attribute and an unknown instance look identical, and painting a whole file red would be
the wrong answer to "I have not checked the metadata in".

### What it will not guess

SFCC script is untyped, so the object type is inferred from the variable name —
`apiProduct`, `currentBasket` and `productLineItem` each resolve to one. The table is
deliberately narrow:

- A name it does not recognise is left alone entirely. No completion, no warning.
- A name that does not decide — `paymentInstrument` is an order's or a customer's — keeps
  **both** types, and only warns when the attribute is in neither.
- `address` on its own is not mapped at all, because a custom object is very often called
  `..._address` and its attributes are its own.
- `hasOwnProperty` and friends are JavaScript, not metadata, and `__`-prefixed names are
  the platform's.
- `getCustomPreferenceValue('logo_' + locale)` names no single preference, so it is
  not checked.

In practice the guard is narrow enough to stay quiet: across a large codebase only a
handful of accesses are flagged, and each one is a real absence — dead code, or an
attribute that never made it into the metadata.

## The `dw.*` API

There is no `.d.ts` in an SFCC checkout and no package to resolve, so nothing can tell an
editor what `dw/system/Site` has on it. The platform reference is compiled into the
server instead: **451 classes, 3.876 methods**, generated from the markdown that ships in
the `sfcc-dev-mcp` package by `examples/generate-api.rs`.

```
var Site = require('dw/system/Site');
var id = Site.getCurrent();
//            ^ hover
//            dw.system.Site
//            static getCurrent() : Site
//
//            Returns the current site.
```

Completion after the dot works off the `require` bindings in the open document, so it
follows whatever you named the import. Hover works on the member and on the module path
in the `require` itself.

Nothing is fetched at run time and there is no `node_modules` in the loop; the index is
parsed on the first question and never if you do not ask one. Regenerating it after a
platform release is one command, in the header of `generate-api.rs`.

## What it checks in the configuration files

A form definition and a `steptypes.json` are read by the platform at run time, so a typo
in a resource key surfaces as a raw key on the page, and a wrong `module` path as a job
that fails the first time someone runs it. Both are decidable from the checkout.

| File | Checked |
| ---- | ------- |
| `cartridge/forms/**/*.xml` | `label`, `missing-error`, `range-error`, `value-error` and `parse-error` name a key some default bundle defines |
| `steptypes.json` | every `module` resolves to a file — extension optional, as the platform allows — and no `@type-id` is declared twice |

A `label` is often a literal — a month number, a card brand, a place name — so only a
dotted, unspaced value is treated as claiming to be a resource key. And the key has to
exist in **some** bundle, not in `forms`: a quarter of this codebase's form labels live
in another one, and demanding the conventional bundle would be 22 false alarms.

Both find real absences: on a mature codebase, dozens of form keys that no bundle
defines, and job steps pointing at modules that are not there.

> Diagnostics on these two need the server attached to XML and JSON, which is what
> `languages` in `extensions/isml/extension.toml` now asks for. Zed has to know a language by
> that name for it to take effect; if the forms stay quiet, that is the thing to check.

## Which `server.append` actually runs

A route is assembled from every cartridge in the path that declares it, and the file being
edited says nothing about the others. Hover a route name — in `server.append('Show', ...)`,
or an endpoint written out in full like `URLUtils.url('Account-Show')` — and the whole chain
comes back, one table per storefront:

```
Account-Show

storefront
  1  app_brand            replace  runs
  2  int_payment          append   never reached
  3  int_oms              append   never reached  <- this file
  4  app_storefront_base  get      never reached
  5  app_other_brand      replace  not in this path

app_brand replaces this route, so int_payment never runs.
```

`Go to Definition` on the same name walks the chain too, what runs offered first.

Three things decide a link's fate, and all three are invisible from inside one file:

- **`replace` discards the cartridges to its right** — but not an `append` further down its
  own file, which attaches to the route the `replace` just installed.
- **A controller that never extends its `module.superModule` cuts the chain**: nothing to
  its right is loaded at all. Both idioms count — `server.extend(module.superModule)` and
  the more common `var page = module.superModule; server.extend(page);`.
- **A cartridge outside this site's path is not in the running**, which is how the same
  route reads differently for two storefronts.

### Where the order comes from

Whichever of these the folder holds, all of them used:

| Source | Gives |
| ------ | ----- |
| `<custom-cartridges>` in a site archive (`.../sites/<site>/site.xml`) | one ordered path per storefront, labelled with the site id |
| the `cartridge` array of `dw.json` | one path, labelled `dw.json` |

Only the `cartridge` key of `dw.json` is read; every other field in it is a credential, and
an unknown field is dropped rather than held.

With neither, the chain is still listed — every cartridge that declares the route — but
unordered, and it says so instead of guessing who wins.

## Is the sandbox running this code?

A detached uploader is invisible from inside the editor, so a save that failed to reach
the sandbox looks exactly like one that worked — and the next half hour goes into
debugging code the instance never received.

The uploader writes a status file per watcher; the server follows it and reports through
LSP progress, which Zed renders in its status bar:

```
SFCC   uploading 7 files to sbx-001.example.com
SFCC   upload failed — 502 Bad Gateway
```

**There is no always-on green light.** Progress is meant for work in flight, and an item
that never ends reads as a stuck spinner, so the two states worth interrupting for are
shown — uploading, and failed until an upload succeeds — and nothing at all when
everything is in sync. No news is good news.

The file is keyed by the cartridges directory, so a workspace finds its own watcher
without having to reproduce how the sandbox identity is derived. A watcher that stops
writing for ninety seconds is treated as gone rather than quiet.

## Highlighting

The grammar parses 98.9% of a large corpus of ISML templates without a single error node; the remainder are templates with genuinely unbalanced markup (a stray `</div>`,
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
tools\package.ps1 -ExtensionDir extensions\isml -Binary <path-to>\isml-lsp.exe
tools\package.ps1 -ExtensionDir extensions\b2c-debug        # the other extension
```

Either way the payload is only what Zed loads at runtime — `extension.toml`,
`extension.wasm`, `grammars\*.wasm`, `languages\` — plus `tools/packaging/install.ps1`, which both
routes copy verbatim so the two zips cannot drift apart. `tools/package.ps1` needs an
`extension.wasm`, and Zed writes that when you run `zed: install dev extension`, so install
the extension here at least once before packaging locally.

Note that installing the package on this machine replaces the dev-extension symlink with
a static copy; re-run `zed: install dev extension` to go back to developing.

## Releasing

`isml-lsp` binaries are what a user without Rust gets, so a release is a tag:

```powershell
tools\install-lsp.ps1 -PinGrammar   # only if grammar\ changed since the last release
# commit the manifest, then
git tag v0.1.0 ; git push --tags
```

`release.yml` builds `isml-lsp` for Windows, macOS and Linux (x86_64 and aarch64) and
attaches the archives with the names `download_binary` in `extensions/isml/src/isml.rs`
expects. Keep those two in sync.

Switching the grammar between the local copy and this repo leaves a checkout in
`extensions/isml/grammars/` pointing at the old URL, and Zed refuses to reuse it —
*"already exists, but is not a git clone of ..."*. `install-lsp.ps1` deletes it whenever it
rewrites the pointer; if you edit `extension.toml` by hand, delete it yourself.

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

The language server is a library with a two-screen binary on top, so `cargo doc`
has something to document and every public item carries a doc comment
(`#![warn(missing_docs)]` keeps it that way). CI publishes it to GitHub Pages on
every push to `main`, with `RUSTDOCFLAGS=-D warnings` so a broken intra-doc link
never ships as a dead link.

```bash
cd ../../grammar
npx tree-sitter generate                                     # after editing grammar.js
bash check.sh                                                # fixtures + query load
bash check.sh path/to/cartridges                             # ...and a corpus of your own
CC=/c/rust/zig/clang.cmd npx tree-sitter parse some.isml     # inspect one parse tree

cd ../crates/isml-lsp
cargo test
cargo doc --no-deps --lib --open                             # what CI publishes
node smoke-test.mjs <workspace-root> <file.isml> "<needle>"  # end-to-end over stdio
```

### Why this lives in `C:\dev` and not in `~\Github`

**It has to.** Zed compiles an extension in place and hardcodes the target directory:

```rust
// crates/extension/src/extension_builder.rs
.arg("--target-dir").arg(extension_dir.join("target"))
```

A Windows profile with an accented character in it — `C:\Users\Renée`, say — defeats the
MinGW linker, which cannot resolve such a path. Under that profile the host proc-macros
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
