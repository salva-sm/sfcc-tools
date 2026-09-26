#!/usr/bin/env bash
# Usage: ./check.sh [path/to/cartridges]  (a directory also measures a real corpus)
set -e

here="$(cd "$(dirname "$0")" && pwd)"
# The Windows dev machine's only C compiler is zig, behind a shim.
if [ -z "${CC:-}" ] && [ -f /c/rust/zig/clang.cmd ]; then
    export CC=/c/rust/zig/clang.cmd
fi
cd "$here"

echo "==> fixtures"
summary=$(npx tree-sitter parse --quiet --stat test/fixtures/*.isml | grep "Total parses")
echo "    $summary"
case "$summary" in
    *"failed parses: 0;"*) ;;
    *) echo "FAIL: a fixture does not parse"; exit 1 ;;
esac

echo "==> queries"
for query in ../extensions/isml/languages/isml/*.scm; do
    npx tree-sitter query --quiet "$query" test/fixtures/syntax.isml >/dev/null
    echo "    ok $(basename "$query")"
done

if [ -n "$1" ]; then
    echo "==> corpus $1"
    npx tree-sitter parse --quiet --stat "$1/**/*.isml" | grep "Total parses"
fi

echo "OK"
