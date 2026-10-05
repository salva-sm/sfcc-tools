# sfcc-tui

One terminal screen for the other tools here: the uploader's watchers and their sandboxes,
the errors `log-diff` has pending, and the debug sessions. Optional: nothing needs it, and it
needs nothing running. It reads the state the tools already write for the editor, and runs
them for anything that acts.

```
┌ Watchers ─────────────────────────────────────────────────────────────────────────┐
│  STATE      SANDBOX    ODS        VERSION          CHECKOUT     BEAT              │
│▶ synced     sbx-001    started    version1         shop         7s                │
└───────────────────────────────────────────────────────────────────────────────────┘
┌ SFCC errors · log-diff ──────────────────────────────┐┌ Debugging ────────────────┐
│LOG-DIFF   SANDBOX    CHECKOUT   PENDING  NEW  CHECKED││STATE     CHECKOUT         │
│watching   sbx-001    shop       2        1    4s ago ││attached  shop             │
└──────────────────────────────────────────────────────┘└───────────────────────────┘
┌ Uploads · version1 ───────────────────────────────────────────────────────────────┐
│[09:56:19] OK  version1 already up to date                                         │
│[10:02:41] ->  app_shop/cartridge/controllers/Cart.js uploaded                     │
└───────────────────────────────────────────────────────────────────────────────────┘
 Tab focus  ↑↓ move  l sandbox log  d log-diff  w watch errors  p push  x stop  e errors
 S stop sandbox  R restart sandbox  q quit
```

## Keys

The bottom lines list only the keys that do something here, saying what: those of the
panel shown, and for the chosen watcher - `s` for a stopped one and `x` for a running one,
`w` says whether it starts or stops the errors watcher. Scrolling (`↑↓`, `PgUp` `PgDn`) is
listed only when the panel holds more lines than it shows, and `End` once it is scrolled up.
With one watcher there is nothing for the arrows to choose, so they scroll the panel
whichever has the focus. A key that is not listed does nothing.

Always there:

| Key | |
| :-- | :-- |
| `Tab` | Focus the watchers or the panel below |
| `↑` `↓` / `j` `k` | Choose a watcher, or scroll the panel. `PgUp` `PgDn` by page, `End` to follow |
| `a` | The panel shows the watcher's uploads |
| `l` | The panel shows the sandbox log: `sfcc-upload logger`, run inside, stopped with the TUI. A `●` after it says that log is being followed |
| `o` | The panel shows the sandbox's state as ODS reports it, asked again when it opens. Only for an on-demand sandbox |
| `d` | The panel shows the log of `log-diff start`'s watcher |
| `q` / `Esc` / `Ctrl-C` | Quit |

Only in the panel they belong to:

| Panel | Key | |
| :-- | :-- | :-- |
| Uploads (`a`) | `s` / `x` | `sfcc-upload start` / `stop` for the chosen watcher's checkout |
| | `p` | `sfcc-upload push`, with the terminal handed over so it can ask before overwriting |
| log-diff (`d`) | `w` | `log-diff start`, or `stop` when it runs |
| | `e` | `log-diff list --pending`, the same way |
| Sandbox (`o`) | `S` | Start the watcher's on-demand sandbox when it is stopped, stop it when it is started |
| | `R` | Restart it, when it is started |

Stopping or restarting the sandbox takes its key twice: it goes down for everyone using it.
Starting it does not, but spends realm credits while it runs - hence upper case for both.

The LOG-DIFF column says `watching` for a watcher `log-diff start` left running, `in a task`
for one in a terminal or an editor task, `stopped` otherwise. A debug session whose adapter no
longer answers shows as `gone`.

## The ODS column

The state of the watcher's sandbox as the Sandbox API reports it, through
`sfcc-upload sandbox` run in the background: every minute while it is settled, every ten
seconds while it is starting or stopping. It is red when the sandbox cannot take an upload,
and `…` follows it while a run is under way. Empty for a host that is not an on-demand
sandbox; `unknown` when ODS could not be asked, with the reason on the notice line. The API
client of `dw.json` needs the Sandbox API User role on the realm; see
[`sfcc-upload`](../sfcc-upload/README.md#the-sandbox-itself).

`sfcc-tui --print` draws one frame to stdout and exits, without asking ODS.

## Install

From the [latest release](https://github.com/salva-sm/sfcc-tools/releases/latest), or:

```bash
cargo install --path crates/sfcc-tui
```

It runs `sfcc-upload` and `log-diff` from the `PATH`. Windows Terminal or PowerShell draw it
properly; Git Bash's own window may not.
