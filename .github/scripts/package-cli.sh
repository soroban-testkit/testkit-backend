#!/usr/bin/env bash
#
# Packages one built soroban-testkit CLI binary for a release target.
#
# Usage:
#   bash .github/scripts/package-cli.sh <version> <target> <binary>
#
#   bash .github/scripts/package-cli.sh 0.1.0 x86_64-apple-darwin \
#     target/x86_64-apple-darwin/release/soroban-testkit
#
# Writes dist/soroban-testkit-<version>-<target>.tar.gz and a matching
# .sha256 file beside it. Checks the target and the binary before writing
# anything, so a bad input fails the release job before any upload.

set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <version> <target> <binary>" >&2
  exit 2
fi

version="${1#v}"
target="$2"
binary="$3"

case "$target" in
  x86_64-unknown-linux-gnu | x86_64-apple-darwin | aarch64-apple-darwin | x86_64-pc-windows-msvc)
    ;;
  *)
    echo "error: '$target' is not a supported release target." >&2
    echo "supported: x86_64-unknown-linux-gnu x86_64-apple-darwin aarch64-apple-darwin x86_64-pc-windows-msvc" >&2
    echo "To add one, update this list and the matrix in .github/workflows/release.yml together." >&2
    exit 1
    ;;
esac

if [ -z "$version" ]; then
  echo "error: version is empty; pass the release version, for example 0.1.0." >&2
  exit 1
fi

if [ ! -f "$binary" ]; then
  echo "error: no binary at '$binary'." >&2
  echo "Build it first: cargo build --release --locked -p soroban-testkit-cli --target $target" >&2
  exit 1
fi

exe="soroban-testkit"
case "$target" in
  *-windows-*) exe="soroban-testkit.exe" ;;
esac

name="soroban-testkit-$version-$target"
dist="$(pwd)/dist"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

hash_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi
}

mkdir -p "$dist"
cp "$binary" "$staging/$exe"
tar -C "$staging" -czf "$dist/$name.tar.gz" "$exe"
(cd "$dist" && hash_file "$name.tar.gz" > "$name.tar.gz.sha256")

echo "packaged $dist/$name.tar.gz"
