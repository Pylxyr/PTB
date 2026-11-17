#!/usr/bin/env bash
# The binary that ships must be static (it runs on a server with nothing installed for it) and must report the
# version it was built as.
#   scripts/check-binary.sh <path-to-binary> [expected-version]
set -euo pipefail

bin="${1:?usage: check-binary.sh <binary> [expected-version]}"
expected="${2:-}"

[ -x "$bin" ] || { echo "::error::$bin does not exist or is not executable"; exit 1; }
echo "size: $(( $(stat -c %s "$bin") / 1024 )) KiB"

if readelf -d "$bin" 2>/dev/null | grep -q '(NEEDED)'; then
  echo "::error::$bin is dynamically linked; it needs:"
  readelf -d "$bin" | grep '(NEEDED)'
  exit 1
fi
echo "static: no shared libraries needed"

reported="$("$bin" --version)"
echo "reports: $reported"
if [ -n "$expected" ] && [ "$reported" != "pyxeetb $expected" ]; then
  echo "::error::expected 'pyxeetb $expected' but the binary says '$reported'"
  exit 1
fi
