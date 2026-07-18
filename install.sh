#!/usr/bin/env sh
# Leafmask installer.
#
#   curl -fsSL https://raw.githubusercontent.com/OWNER/leafmask/main/install.sh | sh
#
# Environment overrides:
#   LEAFMASK_REPO="OWNER/leafmask"   GitHub repo (owner/name)
#   LEAFMASK_VERSION="v0.1.0"        specific tag (default: latest release)
#   LEAFMASK_INSTALL_DIR="/usr/local/bin"   install location
set -eu

REPO="${LEAFMASK_REPO:-OWNER/leafmask}"
BIN="leafmask"

info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
err() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

# --- pick a downloader -----------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fsSL "$1"; }
  dl_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -qO- "$1"; }
  dl_to() { wget -qO "$2" "$1"; }
else
  err "need curl or wget"
fi

# --- detect target triple --------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)  os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *) err "unsupported OS '$os' — build from source instead" ;;
esac
case "$arch" in
  x86_64|amd64)  arch_part="x86_64" ;;
  aarch64|arm64) arch_part="aarch64" ;;
  *) err "unsupported architecture '$arch' — build from source instead" ;;
esac
target="${arch_part}-${os_part}"

# --- resolve version -------------------------------------------------------
version="${LEAFMASK_VERSION:-}"
if [ -z "$version" ]; then
  info "resolving latest release of $REPO"
  version="$(dl "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep -m1 '"tag_name"' | cut -d'"' -f4)"
  [ -n "$version" ] || err "could not determine the latest release (set LEAFMASK_VERSION)"
fi
info "installing $BIN $version ($target)"

# --- download + verify -----------------------------------------------------
base="https://github.com/${REPO}/releases/download/${version}"
archive="${BIN}-${version}-${target}.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

dl_to "${base}/${archive}" "${tmp}/${archive}" \
  || err "download failed: ${base}/${archive} (no prebuilt binary for $target?)"

# Checksum verification when the checksums file and a sha tool are available.
if dl_to "${base}/${BIN}-${version}-checksums.txt" "${tmp}/checksums.txt" 2>/dev/null; then
  if command -v sha256sum >/dev/null 2>&1; then sha="sha256sum";
  elif command -v shasum >/dev/null 2>&1; then sha="shasum -a 256"; else sha=""; fi
  if [ -n "$sha" ]; then
    want="$(grep " ${archive}\$" "${tmp}/checksums.txt" | awk '{print $1}')"
    got="$(cd "$tmp" && $sha "$archive" | awk '{print $1}')"
    [ -n "$want" ] && [ "$want" = "$got" ] || err "checksum mismatch for $archive"
    info "checksum verified"
  fi
fi

tar -xzf "${tmp}/${archive}" -C "$tmp"
[ -f "${tmp}/${BIN}" ] || err "archive did not contain '$BIN'"
chmod +x "${tmp}/${BIN}"

# --- install ---------------------------------------------------------------
dir="${LEAFMASK_INSTALL_DIR:-/usr/local/bin}"
if [ -w "$dir" ] 2>/dev/null || mkdir -p "$dir" 2>/dev/null && [ -w "$dir" ]; then
  mv "${tmp}/${BIN}" "${dir}/${BIN}"
elif command -v sudo >/dev/null 2>&1; then
  info "installing to $dir (needs sudo)"
  sudo mv "${tmp}/${BIN}" "${dir}/${BIN}"
else
  dir="${HOME}/.local/bin"
  mkdir -p "$dir"
  mv "${tmp}/${BIN}" "${dir}/${BIN}"
  info "installed to $dir — add it to your PATH"
fi

info "installed: $("${dir}/${BIN}" --version 2>/dev/null || echo "${dir}/${BIN}")"
