#!/usr/bin/env bash
# Regression check: every fixture must parse without a single ERROR node, and
# the highlight/injection queries must load against the grammar.
#
# Pass a directory to also measure a real corpus, e.g.
#   ./check.sh ~/Github/sfcc-eu/source/cartridges
set -e

here="$(cd "$(dirname "$0")" && pwd)"
# On the Windows dev machine the only C compiler is zig, behind a shim; on CI
# the platform compiler is used.
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
for query in ../extension/languages/isml/*.scm; do
    npx tree-sitter query --quiet "$query" test/fixtures/syntax.isml >/dev/null
    echo "    ok $(basename "$query")"
done

if [ -n "$1" ]; then
    echo "==> corpus $1"
    npx tree-sitter parse --quiet --stat "$1/**/*.isml" | grep "Total parses"
fi

echo "OK"
