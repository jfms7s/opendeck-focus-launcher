#!/usr/bin/env bash
# Checks that a built macOS binary is an Apple Silicon executable and, on a
# Mac, that it is signed and starts. Used by ci.yml's `macos` job and by
# release.yml's `build-macos` and `build` jobs, so the check lives in one place.
#
# Usage: scripts/check-macos-binary.sh <path> [--static]
#   --static  only the checks that work off a Mac (on the Ubuntu runner that
#             unpacks the binary): executable bit and Mach-O arm64 file type.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 || ( $# -eq 2 && "$2" != "--static" ) ]]; then
  echo "usage: $0 <path> [--static]" >&2
  exit 2
fi
bin=$1
static=${2:-}

test -x "$bin"
file "$bin"
# macOS's file says "executable arm64", Linux's "arm64 executable".
file -b "$bin" | grep -Eq '^Mach-O 64-bit (arm64 executable|executable arm64)'

if [[ "$static" == "--static" ]]; then
  exit 0
fi

# Apple Silicon kills unsigned binaries; the linker's ad-hoc signature must
# survive `strip = true`.
codesign --verify --verbose "$bin"
# Without OpenDeck's arguments it stops at the first missing flag, which
# proves it got as far as main().
out=$("$bin" 2>&1 || true)
echo "$out"
echo "$out" | grep -q "missing CLI flag: -port"
