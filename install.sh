#!/bin/sh
# Installs Haul: downloads the release for this machine, checks it against its published
# checksum, and runs `haul setup` (which puts Haul into ~/.haul/bin, first on PATH, and installs
# stable Rust). Arguments are passed to `haul setup` (`--no-modify-path`, `--toolchain none`).
#
#   curl -fsSL https://github.com/jowharshamshiri/haul/releases/latest/download/install.sh | sh
set -eu

releases="${HAUL_UPDATE_URL:-https://github.com/jowharshamshiri/haul/releases/latest/download}"

fail() {
    echo "haul install: $*" >&2
    exit 1
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-musl ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    *) fail "no release for $(uname -s) $(uname -m); build Haul from source" ;;
esac

download() {
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL --retry 3 "$1" -o "$2" || fail "could not download $1"
    elif command -v wget > /dev/null 2>&1; then
        wget -q "$1" -O "$2" || fail "could not download $1"
    else
        fail "curl or wget is needed"
    fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
download "$releases/haul-$target.tar.gz" "$tmp/haul.tar.gz"
download "$releases/haul-$target.tar.gz.sha256" "$tmp/haul.tar.gz.sha256"
want="$(cut -d ' ' -f 1 < "$tmp/haul.tar.gz.sha256")"
if command -v sha256sum > /dev/null 2>&1; then
    got="$(sha256sum "$tmp/haul.tar.gz" | cut -d ' ' -f 1)"
else
    got="$(shasum -a 256 "$tmp/haul.tar.gz" | cut -d ' ' -f 1)"
fi
[ "$want" = "$got" ] || fail "the download is not the published file (checksum $got, published $want)"
tar -xzf "$tmp/haul.tar.gz" -C "$tmp"
"$tmp/haul-$target/haul" setup "$@"
