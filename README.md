# prost

Cartridge uploader for SFCC sandboxes, as a CLI. Same job as the Prophet VS Code extension —
keep a code version in sync with the cartridges on disk — with no editor attached, so Zed,
Neovim or VS Code are all equally fine and closing the editor never stops the upload. The name
is Prophet plus Rust.

Reads the same `dw.json` as Prophet, talks WebDAV, uploads only what changed.

## Install

Download the binary for your platform from the
[latest release](https://github.com/salva-sm/prost/releases/latest) and put it on your
`PATH`. No toolchain needed; the download URL is stable, so this always gets the newest:

```powershell
# Windows
curl -L -o prost.exe https://github.com/salva-sm/prost/releases/latest/download/prost-x86_64-windows.exe
```

```bash
# macOS (Apple silicon) / Linux — swap aarch64 for x86_64 as needed
curl -L https://github.com/salva-sm/prost/releases/latest/download/prost-aarch64-macos.tar.gz | tar -xz
```

To build it yourself instead, see [Building](#building).

## Usage

Run it anywhere inside the repository; `dw.json` is found by walking up.

```bash
prost push                # upload what changed since the last sync
prost push --full         # start over: replace every cartridge on the sandbox
prost push --dry-run      # list what would go up, without touching the sandbox
prost watch               # push, then upload on every save (foreground)
prost start               # same, detached: survives closing the editor
prost start --reload      # same, and reload the storefront tab after each upload
prost stop                # stop the detached watcher
prost status              # watcher, sandbox and local sync state
prost activity -f         # what the watcher has been uploading
prost logger              # follow the sandbox log, where server-side errors land
prost doctor              # check dw.json, connectivity, credentials, code version
prost versions            # code versions on the sandbox
prost ls [PATH]           # what the code version holds on the sandbox
prost rm <PATH>           # delete something left behind up there
prost clean               # delete this project's cartridges from the code version
prost activate [NAME]     # make a code version the active one
prost install-hook        # push automatically after a branch switch
```

`prost --help` lists the commands, `prost help <command>` explains one.

| Option | Effect |
| ------ | ------ |
| `-c, --config <PATH>` | Use this `dw.json` instead of the nearest one |
| `--code-version <NAME>` | Target another code version without editing `dw.json` |
| `--cartridge <NAME>` | Limit the sync to these cartridges (repeatable) |
| `-j, --jobs <N>` | Parallel uploads (default 4) |
| `--allow-shared-instance` | Write to an instance that is not a developer sandbox (never staging or production) |
| `--reload` | On `watch`/`start`: reload the storefront tabs after uploading an `.isml`, `.css` or `.js`. Off unless asked for |
| `--reload-port <PORT>` | DevTools port to reload through (default 9222) |

## What it does not tell you

The tool moves bytes; it never reads the code it uploads, so it cannot report a syntax error.
Three different things report three different failures:

| Failure | Who reports it |
| ------- | -------------- |
| SCSS or client-JS that does not compile | `npm run dev` (webpack), which produces `cartridge/static/` in the first place |
| The upload itself failing | `prost` — HTTP status, retries, and the sandbox being asleep |
| A controller, hook or ISML blowing up at runtime | the sandbox log: `prost logger` |

`npm run dev` and `prost` are complements, not alternatives: webpack compiles, the watcher
picks up what it wrote and sends it.

`prost logger` follows today's log files over WebDAV from the end, `error`, `customerror` and
`custom` by default (`--level all` for everything, `--level warn,error` to pick, `-n 100` to
open with some history). A file the sandbox opens mid-session — the first error of the day
lands in a brand new one — is read whole, not from its end.

A record is printed as the sandbox wrote it: the line carrying the timestamp, and everything
below it indented under it, so a stack trace stays one block instead of twenty entries. Every
file is read on each poll, and what they yield is ordered by the sandbox's own timestamp
before it reaches the screen.

Levels are coloured — `error` red, `warn` yellow, the `custom*` family cyan — and the local
path of a rewritten frame is highlighted. Colour is on for a terminal and off for a pipe;
`--color always` keeps it through one, `--color never` and `NO_COLOR` turn it off.

Stack frames are rewritten into local paths on the way out, so

```
at app_common_brand/cartridge/controllers/Account.js:99 (anonymous)
```

is printed as `…/source/cartridges/app_common_brand/cartridge/controllers/Account.js:99`,
which VS Code, Zed and Windows Terminal turn into a link straight to that line. Only frames
whose file actually exists locally are rewritten; anything else is left untouched.

## Reloading the browser

`--reload` talks to Chrome over the DevTools protocol, so Chrome has to be started with the
port open:

```bash
chrome --remote-debugging-port=9222
```

Then `prost start --reload`. After each successful upload that touched an `.isml`, `.css`
or `.js`, every tab whose URL contains the sandbox hostname is reloaded — other tabs are left
alone. Without the flag nothing connects to the browser at all.

## Activating a code version

WebDAV cannot switch the active code version, so `activate` goes through the OCAPI Data API:

```bash
prost push --code-version release_42 --activate
prost activate release_42
```

It needs an API client in `dw.json` — either `client-id`/`client-secret`, or the
`custom-sfcc-ci` block other SFCC tooling uses — **and** that client has to
list `/code_versions` in the sandbox's *Open Commerce API Settings* (type `Data`, context
`Global`). Without that the sandbox answers 403 and the tool says so.

## Pushing on a branch switch

```bash
prost install-hook
```

Writes a `post-checkout` hook that runs `prost push` after a branch checkout, so the
sandbox follows the branch even when the watcher is not running. It costs 0,4 s when nothing
changed. An existing hook is never replaced unless `--force` is given.

## dw.json

| Key | Meaning |
| --- | ------- |
| `hostname` | Sandbox host. Required |
| `username` / `password` | WebDAV credentials. Required unless `client-id` is used |
| `client-id` / `client-secret` | Account Manager client: used for WebDAV when there is no user/password, and always for `activate` |
| `custom-sfcc-ci` | The `sfcc-oauth-client-id` / `-secret` pair, as sfcc-ci stores it |
| `code-version` | Code version folder. Defaults to `version1` |
| `cartridge` | Cartridges to sync. Omit to sync all of them |
| `cartridgesDir` | Cartridge root relative to `dw.json`. Auto-detected when absent |
| `self-signed` | Accept an invalid TLS certificate |

Optional `.sfccignore` next to the cartridges or next to `dw.json`, one pattern per line:
a bare name skips it anywhere (`fixtures`), `*` plus a suffix skips by extension
(`*.test.js`), anything with a slash is a path prefix (`int_analytics/cartridge/static`).
`node_modules`, `.git`, editor folders and OS junk are always skipped.

## How it works

**Delta.** Each successful upload writes a manifest (size, mtime, xxh3 per file) under
`%LOCALAPPDATA%\prost\`. The next run skips whatever still matches, hashes only the
rest and uploads only real differences — scanning the ~9.500 files of a full checkout takes
about four seconds.

**Batching.** More than a handful of files go up as ~24 MB archives, expanded on the server
with the WebDAV `UNZIP` method and deleted afterwards: one round trip per batch, several in
flight. Small changes go straight up as individual `PUT`s.

**Availability.** Every run probes the code version first. A sleeping sandbox is not an
error — it waits and resumes, queueing the changes it saw meanwhile. Rejected credentials
fail immediately.

**Watching.** Events are debounced 300 ms and coalesced, so a save, a branch switch or a
webpack rebuild becomes one batch. `start` detaches the process, and keeps a log, pid and
heartbeat per sandbox and code version.

## Safety

No external CLI is involved — no sfcc-ci, no b2c-dev-tools, no shelling out. The tool speaks
HTTPS WebDAV directly against
`https://<hostname>/on/demandware.servlet/webdav/Sites/Cartridges/<code-version>/`, the same
endpoint Prophet uses, authenticating every request with the credentials in `dw.json` (Basic,
or a bearer token from Account Manager). Nothing is cached, forwarded or logged: the password
never appears in output, and `doctor` prints only the user name.

Because each request carries the developer's own Business Manager user, the instance
attributes every write to that account — there is no shared or anonymous access path.

Writes are restricted to developer sandboxes. A hostname that looks like staging or
production is refused outright, with no override; anything else that is not a sandbox
(`*.my.commercecloud.salesforce.com`, `*.dx.commercecloud.salesforce.com`) requires
`--allow-shared-instance` on every invocation. Read-only commands (`status`, `doctor`,
`versions`) are never blocked.

## Measured

57 cartridges, 9.493 files, 631,8 MB, `--jobs 4`. The Prophet column was timed with the same
instrument wherever an equivalent operation exists:

| Scenario | prost | Prophet 1.4.81 |
| -------- | --------- | -------------- |
| Cold full deploy of everything | 23,7 s | ~60 s |
| Redeploy with nothing changed | 0,4 s | ~60 s |
| 200 files created at once, watcher | 2,4 s | 6,6 s |
| 200 files deleted at once, watcher | 3,2 s | 2,2 s |
| Save to uploaded, watcher | 0,90 s | 1,05 s |
| One changed file via `push` | 1,3 s | no equivalent |

Method and raw samples: [BENCHMARK.md](BENCHMARK.md).

## Building

Rust 1.85+ (edition 2024):

```bash
cargo test
cargo install --path .        # puts prost on PATH
```

On Windows with the GNU toolchain, MinGW binutils must be on `PATH` (`dlltool`, `as`) and no
path the linker sees may contain non-ASCII characters. On this machine that means MSYS2 at
`C:\msys64\mingw64\bin`, `RUSTUP_HOME`/`CARGO_HOME` under `C:\rust\`, and the `target-dir` in
`.cargo/config.toml`. Sources may stay under the user profile.

## Limitations

- The delta trusts the manifest: if the code version is changed from elsewhere, use `--full`.
- `cartridge/static/` is uploaded, not built — the webpack build still has to run.
- `activate` depends on a BM configuration step per sandbox, so it fails until someone does it.
- Version 0.1.0: exercised end to end against one sandbox, by one person.
