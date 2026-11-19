#!/usr/bin/env bash
# Download the pinned engines for one platform into src-tauri/engines/.
# Usage: scripts/fetch-engines.sh windows|macos|linux
set -euo pipefail

platform="${1:?usage: fetch-engines.sh windows|macos|linux}"
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/src-tauri/engines"
mkdir -p "$dest"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

found=0
while read -r plat file hash url; do
  [[ -z "$plat" || "$plat" == \#* || "$plat" != "$platform" ]] && continue
  found=1
  out="$dest/$file"
  if [[ -f "$out" && "$(sha256 "$out")" == "$hash" ]]; then
    echo "ok      $file (cached)"
    continue
  fi
  echo "fetch   $file <- $url"
  curl -fsSL --retry 3 -o "$out.part" "$url"
  got="$(sha256 "$out.part")"
  if [[ "$got" != "$hash" ]]; then
    rm -f "$out.part"
    echo "error: $file sha256 is $got, expected $hash" >&2
    exit 1
  fi
  mv "$out.part" "$out"
  echo "ok      $file"
done < "$root/engines.lock"

if [[ "$found" == 0 ]]; then
  echo "error: no engines listed for platform '$platform'" >&2
  exit 1
fi
