# B2C Commerce Debugger for Zed

Breakpoints, stepping and variable inspection in server-side Salesforce B2C Commerce scripts —
controllers, hooks, jobs, SCAPI hooks — from inside Zed.

Zed only speaks to debuggers it knows about, and B2C Commerce is not one of them. This
extension registers one: it tells Zed how to launch `b2c debug`, the Debug Adapter Protocol
adapter that ships with the official Salesforce CLI. The debugging itself is done by that
adapter and by the instance's own `dw/debugger/v2_0` API; this repository is the ~150 lines of
glue that makes Zed aware of it.

## Why it talks to the instance directly

The Salesforce CLI ships a DAP adapter of its own, `b2c debug`, and this extension began as
a wrapper around it. That did not work. Measured on b2c-cli 1.23.2 by reading the wire:

| Behaviour | Result |
| --------- | ------ |
| `initialize` | answers with capabilities |
| `initialized` event | **never sent** — an editor that follows the protocol waits for it forever |
| `attach` | answers success |
| `setBreakpoints` | answers success with an **empty** list, and nothing halts |

The wrapper that replaced it worked, but dragged the whole CLI along: Node 20+ at first,
Node 22.16 by the end — more than most SFCC teams have, for a debugger.

So the adapter is now [`sfcc-dap`](../../crates/sfcc-dap), a Rust binary that speaks DAP to
the editor and the instance's own `dw/debugger/v2_0` REST API on the other side. No Node,
no CLI, nothing to install but the binary — which the extension downloads if it is not
already on your `PATH`.

Two things that API makes you work for, and which the adapter handles:

- **It never tells you anything.** No event says a breakpoint was hit; the only way to know
  is to keep asking. The adapter polls for a halted thread and turns that into `stopped`.
- **Sessions are pinned to one app server.** The first response sets a `dwsid` cookie, and
  a later request without it can land on a server that has never heard of your debugger.

## Requirements

| | |
| --- | --- |
| Credentials | Basic auth in `dw.json` — a Business Manager user and password or WebDAV access key. OAuth alone is not enough |
| Instance | *Administration → Development Configuration → Script Debugger → Enable* |

Only one debugger client can attach to an instance at a time: if the Prophet or the Salesforce
VS Code extension is attached, this one cannot be.

## Install

Not in the Zed registry, so there are two ways in.

**From a zip — nothing to build.** Take `b2c-debug-<version>.zip` from
[Releases](https://github.com/salva-sm/sfcc-tools/releases), unzip it anywhere and run
the `install.cmd` inside — Windows blocks a downloaded `.ps1` under the default
execution policy, and the `.cmd` gets past it without changing anything on the machine. It drops the extension into
`%LOCALAPPDATA%\Zed\extensions\installed`, which Zed watches, so it is picked up without a
restart.

**From source, to work on it.**

1. Clone it somewhere with **no non-ASCII characters in the path**. Zed builds the extension
   with cargo *in place*, hardcoding `--target-dir` inside the clone, and the MinGW linker
   fails on accented paths.

   ```bash
   git clone https://github.com/salva-sm/sfcc-tools.git C:/dev/sfcc-tools
   ```

2. In Zed: **Extensions → Install Dev Extension** and pick the clone.

Zed compiles it on install; there is nothing to build by hand. After editing the extension,
use **Extensions → Rebuild** (or reinstall) to pick the change up. `.\package.ps1` turns what
Zed built into the zip above.

### How the adapter is found

In order: the `binary` setting of the debug configuration, then `sfcc-dap` on the `PATH`
Zed inherits, and failing both it is downloaded from this repository's latest release.
Nothing is bundled, so it does not matter how you installed it.

## Use

Add a `.zed/debug.json` to the project:

```json
[
  {
    "label": "SFCC: attach debugger",
    "adapter": "b2c",
    "request": "attach",
    "cartridge_path": "source/cartridges"
  }
]
```

Then set breakpoints in the gutter and start the session. The debugger attaches to the
instance; it never launches anything, so a breakpoint is only hit once a request, job or SCAPI
call runs that code.

| Field | Meaning |
| ----- | ------- |
| `cartridge_path` | Cartridges directory, absolute or relative to the worktree root. Defaults to `source/cartridges` or `cartridges`, whichever holds `modules/server/route.js` |
| `config` | Path to a `dw.json`. Defaults to whatever the CLI resolves from the worktree root |
| `logs` | `false` stops the sandbox log from being followed while attached. On by default |
| `log_level` | Levels followed in the debug console, comma separated, or `all`. Defaults to `error,customerror` |
| `client_id` | Client ID reported to the debugger API. Change it when two people share an instance |
| `raw_variables` | `true` shows every member the engine reports, unfiltered. Off by default |
| `binary` | Path to `sfcc-dap`. Set it when the adapter is not on the `PATH` Zed inherits |

`binary` is machine-specific: only add it when the session fails to start, and prefer
making `sfcc-dap` resolvable by a plain `PATH` lookup so the committed `.zed/debug.json`
stays the same for everyone.

## What you see when it halts

Three scopes, **SFCC** first:

```
SFCC
  pdict      {"CurrentCustomer":"anonymous","Order":"00012345"}
  request    dw.system.Request@3f21
  session    dw.system.Session@11ac
  customer   dw.customer.Customer@77be
  response   dw.system.Response@0a4c
Locals
  req        {"locale":"fr_FR","querystring":{}}
  viewData   {"actionUrl":"/on/demandware.store/Account-Show"}
Closure
  server     {"routes":12}
```

`pdict`, `request`, `session`, `customer`, `response` and `out` are injected by the
platform, so they are in neither the local nor the closure scope — without this, reaching
them means typing into the watch box at every single breakpoint. The scope is built by
asking `typeof` for each name, so a frame that does not have one simply does not list it:
an ISML frame shows `pdict` and `out`, a controller frame shows neither. `dw` is left out
on purpose; it is the whole API namespace and expanding it is never what anyone wanted.

Every value is a summary, not `[object Object]`: dw classes are Java-backed and answer
`String()` with something readable, plain objects answer `JSON.stringify`.

### What is hidden

Expanding a dw object otherwise buries the two fields you want under sixty methods. So:

| Hidden | Where |
| ------ | ----- |
| `constructor`, `prototype`, `class`, `caller`, `callee`, `arguments`, `hasOwnProperty`, `valueOf`, `toString`… | everywhere |
| anything named `__…` | everywhere |
| members whose type is `function` | only inside an expanded object — a local that holds a function is a real local and stays |

`"raw_variables": true` in the debug configuration turns the whole filter off.

The SFCC scope costs one round trip per name to the instance, plus one per object to
summarise it — about ten, once, each time you expand it.

## Reading a stack that crosses cartridges

A stack that goes through four cartridges looks like four unrelated `Account.js` files.
Each frame is labelled with the cartridge it is in, and with the other cartridges holding
the same path:

```
show  ·  app_brand, also in app_storefront_base, int_payment      Account.js:42
```

That second half is usually the answer to "why is my breakpoint never hit": the copy
being edited is not the copy being loaded. Which of them wins needs the cartridge path,
which the debugger API does not carry — the language server answers that statically,
from `<custom-cartridges>` or `dw.json`.

## The sandbox log in the session

Attaching also follows the sandbox log: `sfcc-upload logger` is started with the same `dw.json` and
every line it prints reaches the debug console, coloured, for as long as the session lasts. It
is where the error that did *not* stop at a breakpoint shows up. Only `error` and
`customerror` are followed — a debug session is no place for the whole firehose, and
`log_level` widens it. `sfcc-upload` has to be on the `PATH`; when it is not, the console says so
once and the session carries on. `"logs": false` turns it off.

## Two things that will waste your afternoon

Neither is the extension's fault, both bite everyone debugging B2C Commerce:

- **A page served from cache never runs its controller.** If a breakpoint in a controller is
  never hit, add a unique query parameter to the URL and try again.
- **A password-protected storefront answers 401 before any script runs.** Sandboxes usually
  have storefront protection on; the request has to carry those credentials.

When a session does not start at all, the reason reaches the debug console before the
adapter gives up — most often that the instance refused the credentials, that the script
debugger is switched off in Business Manager, or that another client holds the one slot.

## Exercising the adapter without an instance

`crates/sfcc-dap` carries its own tests, and the parts worth checking — path mapping,
which variables are worth showing, the wire framing — are unit-tested there:

```bash
cargo test -p sfcc-dap
```
