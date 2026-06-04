#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUNDLE_SCRIPT="$SCRIPT_DIR/scripts/bundle-macos.sh"

usage() {
    cat <<'USAGE'
Usage: ./build-installers.sh [--target <target>] [--skip-build]

Creates both macOS installer artifacts:
  - dist/NotMux-<version>-<target>.pkg
  - dist/NotMux-<version>-<target>.dmg

Options are passed through to scripts/bundle-macos.sh.
USAGE
}

ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            if [[ $# -lt 2 ]]; then
                echo "Error: --target requires a value" >&2
                exit 1
            fi
            ARGS+=("--target" "$2")
            shift 2
            ;;
        --skip-build)
            ARGS+=("--skip-build")
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [[ ! -x "$BUNDLE_SCRIPT" ]]; then
    echo "Error: installer script is not executable: $BUNDLE_SCRIPT" >&2
    exit 1
fi

if [[ ${#ARGS[@]} -gt 0 ]]; then
    "$BUNDLE_SCRIPT" "${ARGS[@]}" --pkg --dmg
else
    "$BUNDLE_SCRIPT" --pkg --dmg
fi
