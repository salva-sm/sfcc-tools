# B2C Commerce Debugger for Zed

Breakpoints, stepping and variable inspection in server-side Salesforce B2C Commerce scripts —
controllers, hooks, jobs, SCAPI hooks — from inside Zed.

Zed only speaks to debuggers it knows about, and B2C Commerce is not one of them. This
extension registers one: it tells Zed how to launch `b2c debug`, the Debug Adapter Protocol
adapter that ships with the official Salesforce CLI. The debugging itself is done by that
adapter and by the instance's own `dw/debugger/v2_0` API; this repository is the ~150 lines of
glue that makes Zed aware of it.

## Why it ships its own adapter

The CLI has a DAP adapter of its own, `b2c debug`, and this extension started as a wrapper
around it. That does not work. Measured on b2c-cli 1.23.2 by reading the wire:

| Behaviour | Result |
| --------- | ------ |
| `initialize` | answers with capabilities |
| `initialized` event | **never sent** — an editor that follows the protocol waits for it forever |
| `attach` | answers success |
| `setBreakpoints` | answers success with an **empty** list, and nothing halts. Absolute, cartridge-relative and server paths all behave the same |

The same CLI's RPC mode does every one of those correctly. So `tools/adapter.js` speaks DAP to
the editor and JSONL to `b2c debug cli --rpc`, leaving authentication, cartridge mapping and
halt detection to the CLI. It translates breakpoints, threads, stack frames, scopes,
variables, expression evaluation, stepping and continue, and turns `thread_stopped` into the
DAP `stopped` event.

## Requirements

| | |
| --- | --- |
| CLI | `@salesforce/b2c-cli` on `PATH` (`npm i -g @salesforce/b2c-cli`, or `volta install @salesforce/b2c-cli`) |
| Credentials | Basic auth in `dw.json` — a Business Manager user and password or WebDAV access key. OAuth alone is not enough |
| Instance | *Administration → Development Configuration → Script Debugger → Enable* |

Only one debugger client can attach to an instance at a time: if the Prophet or the Salesforce
VS Code extension is attached, this one cannot be.

## Install

Not in the Zed registry, so there are two ways in.

**From a zip — nothing to build.** Take `b2c-debug-<version>.zip` from
[Releases](https://github.com/salva-sm/sfcc-zed-debugger/releases), unzip it anywhere and run
the `install.cmd` inside — Windows blocks a downloaded `.ps1` under the default
execution policy, and the `.cmd` gets past it without changing anything on the machine. It drops the extension into
`%LOCALAPPDATA%\Zed\extensions\installed`, which Zed watches, so it is picked up without a
restart.

**From source, to work on it.**

1. Clone it somewhere with **no non-ASCII characters in the path**. Zed builds the extension
   with cargo *in place*, hardcoding `--target-dir` inside the clone, and the MinGW linker
   fails on accented paths.

   ```bash
   git clone https://github.com/salva-sm/sfcc-zed-debugger.git C:/dev/sfcc-zed-debugger
   ```

2. In Zed: **Extensions → Install Dev Extension** and pick the clone.

Zed compiles it on install; there is nothing to build by hand. After editing the extension,
use **Extensions → Rebuild** (or reinstall) to pick the change up. `.\package.ps1` turns what
Zed built into the zip above.

### How the CLI is located

The extension never bundles the b2c CLI, it looks for it, so nothing here depends on how you
installed it. In order: the `binary` setting of the debug configuration, then `b2c` on `PATH`
— resolved by the extension and handed to the adapter — then `npm root -g`. Set
`B2C_ADAPTER_ENTRY` to the CLI's `bin/run.js` to override the lot. When none of them work the
adapter says so, in `%TEMP%\b2c-dap.log`.

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
| `instance` | Named instance, when the configuration file holds more than one |
| `logs` | `false` stops the sandbox log from being followed while attached. On by default |
| `log_level` | Levels followed in the debug console, comma separated, or `all`. Defaults to `error,customerror` |
| `client_id` | Client ID reported to the debugger API. Change it when two people share an instance |
| `binary` | Path to the CLI — its `bin/run.js` or the `b2c` executable. Set it when `b2c` is not on the `PATH` Zed inherits |
| `runtime` | Path to the `node` executable that runs `bin/run.js`. Set it when node is managed by Volta, nvm or another shim |

Both paths are machine-specific: only add them when the session fails to start, and prefer
making `b2c` and `node` resolvable to a plain `PATH` lookup so the committed `.zed/debug.json`
stays the same for everyone.

## The sandbox log in the session

Attaching also follows the sandbox log: `prost logger` is started with the same `dw.json` and
every line it prints reaches the debug console, coloured, for as long as the session lasts. It
is where the error that did *not* stop at a breakpoint shows up. Only `error` and
`customerror` are followed — a debug session is no place for the whole firehose, and
`log_level` widens it. `prost` has to be on the `PATH` (`B2C_LOGGER` points at another
binary); when it is not, the console says so once and the session carries on. `"logs": false`
turns it off.

## Two things that will waste your afternoon

Neither is the extension's fault, both bite everyone debugging B2C Commerce:

- **A page served from cache never runs its controller.** If a breakpoint in a controller is
  never hit, add a unique query parameter to the URL and try again.
- **A password-protected storefront answers 401 before any script runs.** Sandboxes usually
  have storefront protection on; the request has to carry those credentials.

Every session is recorded in `b2c-dap.log` in the system temp directory — both the DAP
conversation with Zed and the RPC one with the CLI. `B2C_DAP_LOG` points it elsewhere. That
file is the first place to look when a session does not start.
