#!/bin/bash
set -euo pipefail

# macOS App Bundle Script for NotMux
# Usage: ./scripts/bundle-macos.sh [--target <target>] [--skip-build] [--dmg] [--pkg]

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

export COPYFILE_DISABLE=1

# Defaults
TARGET=""
SKIP_BUILD=false
CREATE_DMG=false
CREATE_PKG=false
RELEASE=false
APP_NAME="NotMux"
BUNDLE_ID="dev.notmux.app"
BIN_NAME="notmux"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --target)
            TARGET="$2"
            shift 2
            ;;
        --skip-build)
            SKIP_BUILD=true
            shift
            ;;
        --dmg)
            CREATE_DMG=true
            shift
            ;;
        --pkg)
            CREATE_PKG=true
            shift
            ;;
        --release)
            RELEASE=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--target <target>] [--skip-build] [--dmg] [--pkg] [--release]"
            echo "--release requires SIGNING_IDENTITY and NOTARY_PROFILE from the local keychain."
            echo "Release mode builds from the matching clean Git tag; it does not upload or publish."
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Detect target if not specified
if [[ -z "$TARGET" ]]; then
    ARCH=$(uname -m)
    if [[ "$ARCH" == "arm64" ]]; then
        TARGET="aarch64-apple-darwin"
    else
        TARGET="x86_64-apple-darwin"
    fi
fi

echo "==> Building $APP_NAME for macOS"
echo "    Target: $TARGET"

cd "$PROJECT_ROOT"

# Get version from Cargo.toml
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
echo "    Version: $VERSION"

case "$TARGET" in
    aarch64-apple-darwin) ASSET_ARCH=arm64 ;;
    x86_64-apple-darwin) ASSET_ARCH=x64 ;;
    *) echo "Unsupported macOS target: $TARGET" >&2; exit 1 ;;
esac

if [[ "$RELEASE" == true ]]; then
    : "${SIGNING_IDENTITY:?Set SIGNING_IDENTITY to your Developer ID Application identity}"
    : "${NOTARY_PROFILE:?Set NOTARY_PROFILE to your notarytool keychain profile}"
    if [[ "$SIGNING_IDENTITY" == "-" || "$SKIP_BUILD" == true || "$CREATE_PKG" == true || "$CREATE_DMG" == true ]]; then
        echo "Release mode requires Developer ID signing and a fresh build; it produces ZIP only (no --pkg/--dmg)." >&2
        exit 1
    fi
    if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "Release mode requires a stable version, e.g. 0.1.0." >&2
        exit 1
    fi
    if [[ -n "$(git status --porcelain)" ]] ||
       [[ "$(git rev-parse "v$VERSION^{commit}")" != "$(git rev-parse HEAD)" ]]; then
        echo "Release mode requires a clean checkout at tag v$VERSION." >&2
        exit 1
    fi
    command -v bun >/dev/null
    xcrun --find notarytool >/dev/null
    xcrun --find stapler >/dev/null
    echo "==> Building embedded web client..."
    (cd web && bun install --frozen-lockfile && bun run build)
fi

# Build if not skipping
if [[ "$SKIP_BUILD" == false ]]; then
    echo "==> Building release binary..."
    cargo build --locked --release --target "$TARGET"
fi

# Verify binary exists (check target-specific path first, then default)
BINARY_PATH="target/$TARGET/release/$BIN_NAME"
if [[ ! -f "$BINARY_PATH" ]]; then
    if [[ "$RELEASE" == true ]]; then
        echo "Missing release binary for $TARGET: $BINARY_PATH" >&2
        exit 1
    fi
    # Try default release path (when built without --target)
    BINARY_PATH="target/release/$BIN_NAME"
    if [[ ! -f "$BINARY_PATH" ]]; then
        echo "Error: Binary not found"
        echo "Checked: target/$TARGET/release/$BIN_NAME"
        echo "Checked: target/release/$BIN_NAME"
        echo "Run without --skip-build or build first with: cargo build --release"
        exit 1
    fi
    echo "    Using default release binary"
fi

# Setup paths
DIST_DIR="$PROJECT_ROOT/dist"
APP_BUNDLE="$DIST_DIR/$APP_NAME.app"
CONTENTS_DIR="$APP_BUNDLE/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
ARTIFACT_DIR="$DIST_DIR"
if [[ "$RELEASE" == true ]]; then
    ARTIFACT_DIR="$DIST_DIR/releases/$VERSION"
    mkdir -p "$ARTIFACT_DIR"
    # Do not leave a previous build's ZIP or manifest looking deployable on failure.
    rm -f "$ARTIFACT_DIR/notmux-macos-$ASSET_ARCH.zip" "$ARTIFACT_DIR/SHA256SUMS"
fi

# Clean and create bundle structure
echo "==> Creating app bundle structure..."
rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS_DIR"
mkdir -p "$RESOURCES_DIR"

# Copy binary
echo "==> Copying binary..."
cp "$BINARY_PATH" "$MACOS_DIR/$BIN_NAME"
chmod +x "$MACOS_DIR/$BIN_NAME"

# Create Info.plist with version
echo "==> Creating Info.plist..."
sed "s/VERSION/$VERSION/g" "$PROJECT_ROOT/macos/Info.plist" > "$CONTENTS_DIR/Info.plist"

# Create icns from PNG icons
echo "==> Creating app icon..."
ICONSET_DIR="$DIST_DIR/AppIcon.iconset"
rm -rf "$ICONSET_DIR"
mkdir -p "$ICONSET_DIR"

# Copy PNG files to iconset with correct naming
cp "$PROJECT_ROOT/assets/app-icon-16.png" "$ICONSET_DIR/icon_16x16.png"
cp "$PROJECT_ROOT/assets/app-icon-32.png" "$ICONSET_DIR/icon_16x16@2x.png"
cp "$PROJECT_ROOT/assets/app-icon-32.png" "$ICONSET_DIR/icon_32x32.png"
cp "$PROJECT_ROOT/assets/app-icon-64.png" "$ICONSET_DIR/icon_32x32@2x.png"
cp "$PROJECT_ROOT/assets/app-icon-128.png" "$ICONSET_DIR/icon_128x128.png"
cp "$PROJECT_ROOT/assets/app-icon-256.png" "$ICONSET_DIR/icon_128x128@2x.png"
cp "$PROJECT_ROOT/assets/app-icon-256.png" "$ICONSET_DIR/icon_256x256.png"
cp "$PROJECT_ROOT/assets/app-icon-512.png" "$ICONSET_DIR/icon_256x256@2x.png"
cp "$PROJECT_ROOT/assets/app-icon-512.png" "$ICONSET_DIR/icon_512x512.png"

# Create 512@2x if we have it, otherwise duplicate 512
if [[ -f "$PROJECT_ROOT/assets/app-icon-1024.png" ]]; then
    cp "$PROJECT_ROOT/assets/app-icon-1024.png" "$ICONSET_DIR/icon_512x512@2x.png"
else
    cp "$PROJECT_ROOT/assets/app-icon-512.png" "$ICONSET_DIR/icon_512x512@2x.png"
fi

# Convert to icns
iconutil -c icns "$ICONSET_DIR" -o "$RESOURCES_DIR/AppIcon.icns"
rm -rf "$ICONSET_DIR"

# Compile the Icon Composer bundle (Assets.car + CFBundleIconName) so macOS 26+
# renders the icon natively instead of applying its full-bleed legacy treatment
# to the .icns. Falls back to the .icns-only bundle when actool is unavailable.
if xcrun --find actool >/dev/null 2>&1; then
    echo "==> Compiling Icon Composer icon (macOS 26+)..."
    ACTOOL_OUT="$DIST_DIR/actool-out"
    rm -rf "$ACTOOL_OUT"
    mkdir -p "$ACTOOL_OUT"
    if xcrun actool "$PROJECT_ROOT/assets/AppIcon.icon" \
        --compile "$ACTOOL_OUT" \
        --platform macosx \
        --minimum-deployment-target 11.0 \
        --app-icon AppIcon \
        --output-partial-info-plist "$ACTOOL_OUT/partial.plist" >/dev/null; then
        cp "$ACTOOL_OUT/Assets.car" "$RESOURCES_DIR/"
    else
        echo "    actool failed, keeping .icns only"
    fi
    rm -rf "$ACTOOL_OUT"
else
    echo "    actool not found, keeping .icns only"
fi

# Create PkgInfo
echo "APPL????" > "$CONTENTS_DIR/PkgInfo"

cp "$PROJECT_ROOT/LICENSE" "$RESOURCES_DIR/LICENSE"
mkdir -p "$RESOURCES_DIR/licenses"
cp "$PROJECT_ROOT/vendor/sum_tree/LICENSE-APACHE" "$RESOURCES_DIR/licenses/sum_tree.txt"
xattr -cr "$APP_BUNDLE" 2>/dev/null || true
find "$APP_BUNDLE" -name '._*' -delete

if [[ "$RELEASE" == true ]]; then
    echo "==> Developer ID signing with hardened runtime..."
    codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" "$APP_BUNDLE"
    codesign --verify --deep --strict "$APP_BUNDLE"
    NOTARY_ZIP="$DIST_DIR/notmux-notary-$ASSET_ARCH.zip"
    NOTARY_RESULT="$DIST_DIR/notmux-notary-$ASSET_ARCH.plist"
    ditto -c -k --sequesterRsrc --keepParent "$APP_BUNDLE" "$NOTARY_ZIP"
    xcrun notarytool submit "$NOTARY_ZIP" --keychain-profile "$NOTARY_PROFILE" \
        --wait --output-format plist > "$NOTARY_RESULT"
    if [[ "$(/usr/libexec/PlistBuddy -c 'Print :status' "$NOTARY_RESULT")" != Accepted ]]; then
        echo "Notarization was not accepted. See $NOTARY_RESULT" >&2
        exit 1
    fi
    xcrun stapler staple "$APP_BUNDLE"
    xcrun stapler validate "$APP_BUNDLE"
    codesign --verify --deep --strict "$APP_BUNDLE"
    spctl --assess --type execute --verbose "$APP_BUNDLE"
    # Recreate the distribution ZIP after stapling, preserving the complete bundle.
    ditto -c -k --sequesterRsrc --keepParent "$APP_BUNDLE" \
        "$ARTIFACT_DIR/notmux-macos-$ASSET_ARCH.zip"
    rm -f "$NOTARY_ZIP" "$NOTARY_RESULT"
else
    echo "==> Ad-hoc code signing (development only)..."
    codesign --force --sign - "$MACOS_DIR/$BIN_NAME"
    codesign --force --sign - "$APP_BUNDLE"
fi

echo "==> App bundle created at: $APP_BUNDLE"

# Create DMG if requested
if [[ "$CREATE_DMG" == true ]]; then
    echo "==> Creating DMG..."
    DMG_NAME="$APP_NAME-$VERSION-$TARGET.dmg"
    DMG_PATH="$DIST_DIR/$DMG_NAME"

    # Remove existing DMG
    rm -f "$DMG_PATH"

    # Create temporary DMG directory
    DMG_TEMP="$DIST_DIR/dmg-temp"
    rm -rf "$DMG_TEMP"
    mkdir -p "$DMG_TEMP"

    # Copy app to temp directory
    cp -R "$APP_BUNDLE" "$DMG_TEMP/"

    # Create symlink to Applications
    ln -s /Applications "$DMG_TEMP/Applications"

    # Create DMG (retry up to 3 times — hdiutil can fail with "Resource busy" on CI)
    for attempt in 1 2 3; do
        if hdiutil create -volname "$APP_NAME" \
            -srcfolder "$DMG_TEMP" \
            -ov -format UDZO \
            "$DMG_PATH"; then
            break
        fi
        echo "    hdiutil failed (attempt $attempt/3), retrying in 5s..."
        sleep 5
        if [[ $attempt -eq 3 ]]; then
            echo "Error: hdiutil create failed after 3 attempts"
            exit 1
        fi
    done

    # Clean up
    rm -rf "$DMG_TEMP"

    echo "==> DMG created at: $DMG_PATH"
fi

# Create installer package if requested
if [[ "$CREATE_PKG" == true ]]; then
    echo "==> Creating installer package..."
    PKG_NAME="$APP_NAME-$VERSION-$TARGET.pkg"
    PKG_PATH="$DIST_DIR/$PKG_NAME"
    PKG_ROOT="$DIST_DIR/pkg-root"

    rm -f "$PKG_PATH"
    rm -rf "$PKG_ROOT"
    mkdir -p "$PKG_ROOT/Applications"
    ditto --norsrc --noextattr "$APP_BUNDLE" "$PKG_ROOT/Applications/$APP_NAME.app"
    find "$PKG_ROOT" -name '._*' -delete

    pkgbuild \
        --root "$PKG_ROOT" \
        --install-location / \
        --identifier "$BUNDLE_ID" \
        --version "$VERSION" \
        "$PKG_PATH"

    rm -rf "$PKG_ROOT"

    echo "==> Installer package created at: $PKG_PATH"
fi

if [[ "$RELEASE" == true ]]; then
    (
        cd "$ARTIFACT_DIR"
        shopt -s nullglob
        ASSETS=(notmux-*.zip notmux-*.tar.gz)
        shasum -a 256 "${ASSETS[@]}" > SHA256SUMS
    )
    echo "==> Release files prepared in $ARTIFACT_DIR (not uploaded or published)."
fi

echo "==> Done!"
echo ""
echo "To install, either:"
echo "  1. Drag '$APP_NAME.app' to /Applications"
echo "  2. Run: cp -R \"$APP_BUNDLE\" /Applications/"
if [[ "$CREATE_PKG" == true ]]; then
    echo "  3. Run: sudo installer -pkg \"$PKG_PATH\" -target /"
fi
