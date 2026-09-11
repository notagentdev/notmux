#!/bin/bash
set -euo pipefail

REPO="notagentdev/notmux"
BOLD="\033[1m"
DIM="\033[2m"
GREEN="\033[32m"
CYAN="\033[36m"
RED="\033[31m"
RESET="\033[0m"

info() { printf "  ${DIM}%s${RESET}\n" "$1"; }
step() { printf "  ${CYAN}>::${RESET} %s\n" "$1"; }
done_msg() { printf "\n  ${GREEN}${BOLD}%s${RESET}\n" "$1"; }
err() { printf "  ${RED}error:${RESET} %s\n" "$1" >&2; exit 1; }

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$ARCH" in
  x86_64) ARCH="x64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  *) err "Unsupported architecture: $ARCH" ;;
esac

case "$OS" in
  darwin|linux) ;;
  *) err "Unsupported OS: $OS. For Windows, use install.ps1" ;;
esac

# Get version (from argument or latest release)
if [ -n "${1:-}" ]; then
  VERSION="${1#v}"
else
  step "Fetching latest version"
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"v([^"]+)".*/\1/') || err "No published release found. Check https://github.com/${REPO}/releases"
fi

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || err "Invalid release version. Usage: install.sh [0.1.0]"
if [[ "$OS" == linux && "$ARCH" != x64 ]]; then
  err "Linux installers currently support x86_64 only."
fi

echo ""
printf "  ${BOLD}NotMux${RESET} ${DIM}v%s${RESET}  ${DIM}%s/%s${RESET}\n" "$VERSION" "$OS" "$ARCH"
echo ""

# Create temp directory
TMP_DIR=$(mktemp -d)
STAGING=""
cleanup() {
  rm -rf "$TMP_DIR"
  if [[ -n "$STAGING" ]]; then
    if [[ -d "$STAGING/previous.app" && ! -e /Applications/NotMux.app ]]; then
      mv "$STAGING/previous.app" /Applications/NotMux.app || true
    fi
    if [[ -d "$STAGING/previous.app" ]]; then
      printf 'Previous app retained at %s/previous.app\n' "$STAGING" >&2
    else
      rm -rf "$STAGING"
    fi
  fi
}
trap cleanup EXIT

verified_download() {
  local asset="$1"
  local base="https://github.com/${REPO}/releases/download/v${VERSION}"
  curl -fsSL "$base/$asset" -o "$TMP_DIR/$asset" || err "No installer for $OS/$ARCH in v$VERSION."
  curl -fsSL "$base/SHA256SUMS" -o "$TMP_DIR/SHA256SUMS" || err "Missing release checksums."
  local expected
  expected=$(awk -v asset="$asset" '$2 == asset { print $1 }' "$TMP_DIR/SHA256SUMS")
  [[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] || err "Missing or invalid checksum for $asset."
  (
    cd "$TMP_DIR"
    if command -v sha256sum >/dev/null; then
      printf '%s  %s\n' "$expected" "$asset" | sha256sum -c -
    else
      printf '%s  %s\n' "$expected" "$asset" | shasum -a 256 -c -
    fi
  ) || err "Checksum verification failed. Installation aborted."
}

if [ "$OS" = "darwin" ]; then
  ARTIFACT="notmux-macos-${ARCH}"
  step "Downloading and verifying"
  verified_download "$ARTIFACT.zip"

  step "Extracting"
  ditto -x -k "$TMP_DIR/$ARTIFACT.zip" "$TMP_DIR"
  [[ -x "$TMP_DIR/NotMux.app/Contents/MacOS/notmux" ]] || err "Archive does not contain NotMux.app."
  [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$TMP_DIR/NotMux.app/Contents/Info.plist")" == dev.notmux.app ]] || err "Unexpected app identifier."
  codesign --verify --deep --strict "$TMP_DIR/NotMux.app" || err "Invalid app signature."
  spctl --assess --type execute "$TMP_DIR/NotMux.app" || err "App is not approved by Gatekeeper."
  if pgrep -f '^/Applications/NotMux.app/Contents/MacOS/notmux([[:space:]]|$)' >/dev/null; then
    err "Quit NotMux before installing the update."
  fi

  step "Installing complete signed app to /Applications"
  STAGING=$(mktemp -d /Applications/.notmux-install.XXXXXX) || err "Cannot write to /Applications."
  ditto "$TMP_DIR/NotMux.app" "$STAGING/NotMux.app"
  codesign --verify --deep --strict "$STAGING/NotMux.app" || err "Staged app signature is invalid."
  if [[ -e /Applications/NotMux.app ]]; then
    mv /Applications/NotMux.app "$STAGING/previous.app"
  fi
  mv "$STAGING/NotMux.app" /Applications/NotMux.app || err "Installation failed; restoring previous app."
  rm -rf "$STAGING/previous.app"

  done_msg "Installed to /Applications/NotMux.app"
  echo ""
  info "Launch from Applications, Spotlight, or run:"
  printf "  ${BOLD}open /Applications/NotMux.app${RESET}\n"
  echo ""

else
  ARTIFACT="notmux-linux-x64"
  step "Downloading and verifying"
  verified_download "$ARTIFACT.tar.gz"
  tar xzf "$TMP_DIR/$ARTIFACT.tar.gz" -C "$TMP_DIR"

  INSTALL_DIR="${HOME}/.local/bin"
  mkdir -p "$INSTALL_DIR"

  step "Installing binary to $INSTALL_DIR"
  mv "$TMP_DIR/notmux" "$INSTALL_DIR/"
  chmod +x "$INSTALL_DIR/notmux"

  step "Installing icons and desktop entry"

  ICON_DIR="${HOME}/.local/share/icons/hicolor"
  for size in 16 32 48 64 128 256 512; do
    mkdir -p "$ICON_DIR/${size}x${size}/apps"
    if [ -f "$TMP_DIR/icons/app-icon-${size}.png" ]; then
      cp "$TMP_DIR/icons/app-icon-${size}.png" "$ICON_DIR/${size}x${size}/apps/notmux.png"
    fi
  done

  mkdir -p "$ICON_DIR/scalable/apps"
  if [ -f "$TMP_DIR/icons/app-icon-simple.svg" ]; then
    cp "$TMP_DIR/icons/app-icon-simple.svg" "$ICON_DIR/scalable/apps/notmux.svg"
  fi

  DESKTOP_DIR="${HOME}/.local/share/applications"
  mkdir -p "$DESKTOP_DIR"
  if [ -f "$TMP_DIR/notmux.desktop" ]; then
    cp "$TMP_DIR/notmux.desktop" "$DESKTOP_DIR/"
  fi

  if command -v gtk-update-icon-cache &> /dev/null; then
    gtk-update-icon-cache -f -t "$ICON_DIR" 2>/dev/null || true
  fi
  if command -v update-desktop-database &> /dev/null; then
    update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true
  fi

  done_msg "Installed successfully"
  echo ""
  info "Binary:  $INSTALL_DIR/notmux"
  info "Desktop: $DESKTOP_DIR/notmux.desktop"
  echo ""

  if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    printf "  ${DIM}Note: Add %s to your PATH:${RESET}\n" "$INSTALL_DIR"
    printf "  ${BOLD}export PATH=\"\$HOME/.local/bin:\$PATH\"${RESET}\n"
    echo ""
  fi

  info "Launch from your application menu or run:"
  printf "  ${BOLD}notmux${RESET}\n"
  echo ""
fi
