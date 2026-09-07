# B2C Commerce Debugger for Zed

Breakpoints, stepping and variable inspection in server-side Salesforce B2C Commerce scripts —
controllers, hooks, jobs, SCAPI hooks — from inside Zed.

Zed only speaks to debuggers it knows about, and B2C Commerce is not one of them. This
extension registers one: it tells Zed how to launch `b2c debug`, the Debug Adapter Protocol
adapter that ships with the official Salesforce CLI. The debugging itself is done by that
adapter and by the instance's own `dw/debugger/v2_0` API; this repository is the ~150 lines of
glue that makes Zed aware of it.

## Status: blocked on the adapter it wraps

`b2c debug` does not hold up its end of the Debug Adapter Protocol. Measured on
b2c-cli 1.23.2 by reading the wire between Zed and the adapter:

| Behaviour | Result |
| --------- | ------ |
| `initialize` | answers with capabilities |
| `initialized` event | **never sent** — a protocol-following editor waits for it forever, which is the spinner Zed shows |
| `attach` | answers success |
| `setBreakpoints` | answers success with an **empty** breakpoint list, and nothing halts. Tried absolute, cartridge-relative and server paths |

The same CLI's RPC mode (`b2c debug cli --rpc`) does all of it correctly: it binds the
breakpoint, reports `thread_stopped` with the thread and location, and resumes on `continue`.

So this extension cannot stay a thin wrapper. `tools/adapter.js` already sits in the right
place — it injects the missing `initialized` event — and the way forward is to grow it into a
DAP-to-RPC translator, leaving authentication, cartridge mapping and halt polling to the CLI.

## Requirements

| | |
| --- | --- |
| CLI | `@salesforce/b2c-cli` on `PATH` (`npm i -g @salesforce/b2c-cli`, or `volta install @salesforce/b2c-cli`) |
| Credentials | Basic auth in `dw.json` — a Business Manager user and password or WebDAV access key. OAuth alone is not enough |
| Instance | *Administration → Development Configuration → Script Debugger → Enable* |

Only one debugger client can attach to an instance at a time: if the Prophet or the Salesforce
VS Code extension is attached, this one cannot be.

## Install

Until it is published, install it as a dev extension:

1. Clone this repository somewhere with **no non-ASCII characters in the path**. Zed builds the
   extension with cargo, and its proc-macro dependencies are linked for the host — the MinGW
   linker fails on accented paths.
2. In Zed: **Extensions → Install Dev Extension** and pick the clone.

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
| `client_id` | Client ID reported to the debugger API. Change it when two people share an instance |

## Two things that will waste your afternoon

Neither is the extension's fault, both bite everyone debugging B2C Commerce:

- **A page served from cache never runs its controller.** If a breakpoint in a controller is
  never hit, add a unique query parameter to the URL and try again.
- **A password-protected storefront answers 401 before any script runs.** Sandboxes usually
  have storefront protection on; the request has to carry those credentials.
