#!/bin/sh
set -eu

REPO="jdblackstar/relay"
BIN="relay"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

need_cmd() {
  command -v "$1" >/dev/null 2>&1
}

verify_sha256() {
  archive_path="$1"
  checksum_path="$2"
  if need_cmd sha256sum; then
    (cd "$(dirname "$archive_path")" && sha256sum -c "$(basename "$checksum_path")")
  elif need_cmd shasum; then
    (cd "$(dirname "$archive_path")" && shasum -a 256 -c "$(basename "$checksum_path")")
  elif need_cmd openssl; then
    expected="$(awk '{print $1; exit}' "$checksum_path")"
    actual="$(openssl dgst -sha256 "$archive_path" | awk '{print $NF}')"
    if [ "$actual" != "$expected" ]; then
      echo "Error: checksum verification failed for $archive_path" >&2
      exit 1
    fi
  else
    echo "Error: sha256sum, shasum, or openssl is required for checksum verification." >&2
    exit 1
  fi
}

fetch() {
  url="$1"
  out="$2"
  if need_cmd curl; then
    curl -fsSL "$url" -o "$out"
  elif need_cmd wget; then
    wget -qO "$out" "$url"
  else
    echo "Error: curl or wget is required." >&2
    exit 1
  fi
}

install_bin() {
  src="$1"
  dst="$2"
  if need_cmd install; then
    install -m 755 "$src" "$dst"
  else
    mkdir -p "$(dirname "$dst")"
    cp "$src" "$dst"
    chmod 755 "$dst"
  fi
}

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-musl" ;;
  *)
    echo "Error: unsupported OS: $os" >&2
    exit 1
    ;;
esac

case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *)
    echo "Error: unsupported architecture: $arch" >&2
    exit 1
    ;;
esac

target="${arch}-${os}"

if [ -n "${RELAY_VERSION:-}" ]; then
  tag="v${RELAY_VERSION#v}"
else
  api="https://api.github.com/repos/${REPO}/releases/latest"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  json="$tmp/latest.json"
  fetch "$api" "$json"
  tag="$(awk -F'"' '/"tag_name":/{print $4; exit}' "$json")"
  if [ -z "$tag" ]; then
    echo "Error: could not determine latest release tag." >&2
    exit 1
  fi
fi

archive="relay_${tag}_${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/${tag}/${archive}"
checksum_url="${url}.sha256"

tmp="${tmp:-$(mktemp -d)}"
trap 'rm -rf "$tmp"' EXIT
fetch "$url" "$tmp/$archive"
fetch "$checksum_url" "$tmp/$archive.sha256"
verify_sha256 "$tmp/$archive" "$tmp/$archive.sha256"

tar -C "$tmp" -xzf "$tmp/$archive"
mkdir -p "$INSTALL_DIR"
install_bin "$tmp/$BIN" "$INSTALL_DIR/$BIN"

echo "Installed $BIN to $INSTALL_DIR/$BIN"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "Add $INSTALL_DIR to your PATH to run: $BIN" ;;
esac
