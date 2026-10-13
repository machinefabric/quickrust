#!/bin/sh
# Installs QuickRust: downloads the release for this machine, checks it against its published
# checksum, and runs `qrust setup` (which puts QuickRust into ~/.qrust/bin, first on PATH, and installs
# stable Rust). Arguments are passed to `qrust setup` (`--no-modify-path`, `--toolchain none`).
#
#   curl -fsSL https://github.com/machinefabric/quickrust/releases/latest/download/install.sh | sh
set -eu

releases="${QRUST_UPDATE_URL:-https://github.com/machinefabric/quickrust/releases/latest/download}"

fail() {
    echo "qrust install: $*" >&2
    exit 1
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-musl ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    *) fail "no release for $(uname -s) $(uname -m); build QuickRust from source" ;;
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
download "$releases/qrust-$target.tar.gz" "$tmp/qrust.tar.gz"
download "$releases/qrust-$target.tar.gz.sha256" "$tmp/qrust.tar.gz.sha256"
want="$(cut -d ' ' -f 1 < "$tmp/qrust.tar.gz.sha256")"
if command -v sha256sum > /dev/null 2>&1; then
    got="$(sha256sum "$tmp/qrust.tar.gz" | cut -d ' ' -f 1)"
else
    got="$(shasum -a 256 "$tmp/qrust.tar.gz" | cut -d ' ' -f 1)"
fi
[ "$want" = "$got" ] || fail "the download is not the published file (checksum $got, published $want)"
tar -xzf "$tmp/qrust.tar.gz" -C "$tmp"
"$tmp/qrust-$target/qrust" setup "$@"
