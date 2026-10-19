#!/usr/bin/env bash
# Builds the soroban-testkit CLI for one release target, then packages it as
# a .tar.gz with a .sha256 checksum in the output directory. The archive
# contains the binary and LICENSE. Usage:
#   build-cli-binaries.sh <target> <output-dir>
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "Usage: $0 <target> <output-dir>" >&2
  exit 1
fi

target="$1"
out_dir="$2"

case "$target" in
  x86_64-unknown-linux-gnu | x86_64-apple-darwin | aarch64-apple-darwin | x86_64-pc-windows-msvc) ;;
  *)
    echo "Error: unsupported release target '$target'." >&2
    echo "Supported targets are listed in RELEASING.md; add a new one there and in release.yml." >&2
    exit 1
    ;;
esac

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$workspace_root/Cargo.toml" | head -1)"

if [ -z "$version" ]; then
  echo "Error: could not read [workspace.package] version from $workspace_root/Cargo.toml" >&2
  exit 1
fi

exe="soroban-testkit"
if [[ "$target" == *windows* ]]; then
  exe="soroban-testkit.exe"
fi

(cd "$workspace_root" && cargo build --release --locked -p soroban-testkit-cli --target "$target")

binary="$workspace_root/target/$target/release/$exe"
if [ ! -f "$binary" ]; then
  echo "Error: expected binary not found at $binary after the release build." >&2
  exit 1
fi

archive="soroban-testkit-v$version-$target.tar.gz"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

cp "$binary" "$staging/$exe"
cp "$workspace_root/LICENSE" "$staging/LICENSE"

mkdir -p "$out_dir"
tar -czf "$out_dir/$archive" -C "$staging" "$exe" LICENSE

# sha256sum on Linux and Windows (Git Bash), shasum on macOS.
if command -v sha256sum >/dev/null 2>&1; then
  sha256=(sha256sum)
else
  sha256=(shasum -a 256)
fi
(cd "$out_dir" && "${sha256[@]}" "$archive" > "$archive.sha256")

echo "Wrote $out_dir/$archive and $out_dir/$archive.sha256"
