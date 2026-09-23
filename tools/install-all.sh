#!/bin/sh
# Downloads every command-line tool from the latest release into one folder.
#
#   ./install-all.sh                    into ~/.local/bin
#   DIR=/usr/local/bin ./install-all.sh somewhere else
#   ./install-all.sh log-diff           just the ones named
#
# Each tool installs on its own as well - this only saves typing the curls.
set -eu

DIR="${DIR:-$HOME/.local/bin}"
BASE=https://github.com/salva-sm/sfcc-tools/releases/latest/download

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) echo "unsupported system: $(uname -s) - on Windows, use install-all.ps1" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

[ "$#" -gt 0 ] || set -- sfcc-upload log-diff isml-lsp sfcc-dap
mkdir -p "$DIR"
for tool in "$@"; do
  curl -fsSL "$BASE/$tool-$arch-$os.tar.gz" | tar -xz -C "$DIR"
  # A first run puts the tab completion where the shell looks for it.
  case "$tool" in sfcc-upload | log-diff) "$DIR/$tool" --version >/dev/null 2>&1 || true ;; esac
  echo "installed $tool"
done

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) echo "$DIR is not on your PATH - add it to your shell profile" ;;
esac
