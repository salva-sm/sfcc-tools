# sfcc-tui

One terminal screen for the other tools here: the uploader's watchers, the errors `log-diff`
has pending, and the debug sessions. Optional: nothing needs it, and it needs nothing
running. It reads the state the tools already write for the editor, and runs them for
anything that acts.

```
┌ Watchers ─────────────────────────────────────────────────────────────────────────┐
│  STATE      SANDBOX    VERSION          CHECKOUT     BEAT                         │
│▶ synced     sbx-001    version1         shop         7s                           │
└───────────────────────────────────────────────────────────────────────────────────┘
┌ SFCC errors · log-diff ──────────────────────────────┐┌ Debugging ────────────────┐
│LOG-DIFF   SANDBOX    CHECKOUT   PENDING  NEW  CHECKED││STATE     CHECKOUT         │
│watching   sbx-001    shop       2        1    4s ago ││attached  shop             │
└──────────────────────────────────────────────────────┘└───────────────────────────┘
┌ Uploads · version1 ───────────────────────────────────────────────────────────────┐
│[09:56:19] OK  version1 already up to date                                         │
│[10:02:41] ->  app_shop/cartridge/controllers/Cart.js uploaded                     │
└───────────────────────────────────────────────────────────────────────────────────┘
 Tab focus  ↑↓ move  a uploads  l sandbox log  d log-diff  w watch errors  p push …
```

## Keys

| Key | |
| :-- | :-- |
| `Tab` | Focus the watchers or the panel below |
| `↑` `↓` / `j` `k` | Choose a watcher, or scroll the panel. `PgUp` `PgDn` by page, `End` to follow |
| `a` | The panel shows the watcher's uploads |
| `l` | The panel shows the sandbox log: `sfcc-upload logger`, run inside, stopped with the TUI |
| `d` | The panel shows the log of `log-diff start`'s watcher |
| `s` / `x` | `sfcc-upload start` / `stop` for the chosen watcher's checkout |
| `w` | `log-diff start`, or `stop` when it runs |
| `p` | `sfcc-upload push`, with the terminal handed over so it can ask before overwriting |
| `e` | `log-diff list --pending`, the same way |
| `q` / `Esc` / `Ctrl-C` | Quit |

The LOG-DIFF column says `watching` for a watcher `log-diff start` left running, `in a task`
for one in a terminal or an editor task, `stopped` otherwise. A debug session whose adapter no
longer answers shows as `gone`.

`sfcc-tui --print` draws one frame to stdout and exits.

## Install

From the [latest release](https://github.com/salva-sm/sfcc-tools/releases/latest), or:

```bash
cargo install --path crates/sfcc-tui
```

It runs `sfcc-upload` and `log-diff` from the `PATH`. Windows Terminal or PowerShell draw it
properly; Git Bash's own window may not.
