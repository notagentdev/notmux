# Desktop releases

Release delivery uses GitHub Releases in `notagentdev/notmux`. No update server is
needed. Mobile builds are disabled. Hosted CI builds Linux x86_64 and Windows
x86_64 into a **draft** release. macOS ZIPs are built, Developer ID signed and
notarized on the release Mac. Publishing is a separate, explicit action.

## Prerequisites

- A clean checkout of the release commit, Rust from `rust-toolchain.toml`, Bun,
  Xcode command-line tools, and the GitHub CLI (`gh`).
- A valid **Developer ID Application** signing identity and its private key in
  this Mac's keychain. An Apple Development certificate alone is not enough.
- A working `notarytool` keychain profile. A signing certificate does not also
  provide notarization authentication. Create a profile interactively if needed:

  ```bash
  xcrun notarytool store-credentials notmux-notary
  ```

  Follow Apple's prompts; do not put passwords, private keys, or certificate
  exports in this repository. If the profile already exists, reuse it.
- `gh` authenticated for the repository. The workflow only needs its normal
  GitHub token; no Homebrew bot or Apple secrets are required in CI.

Before public distribution, resolve the separate license audit findings
(original Okena copyright, third-party font/icon/ported-code notices and binary
license notices). The deployment tooling is not a legal clearance.

## 1. Prepare the version and tag

Update `CHANGELOG.md` with a section such as `## [0.1.0]`. Commit and push the
reviewed source changes first. Then, when intentionally ready to trigger CI:

```bash
bash scripts/tag-version/run.sh 0.1.0
```

**This command can commit the version bump, creates the tag, and pushes.**
It preserves the existing dependency lock resolution. For the first release,
when `Cargo.toml` already has that version, no empty version commit is required.
Use stable `MAJOR.MINOR.PATCH` versions for this release path.

Wait for the `Build` workflow for that tag to succeed. It creates a draft with:

- `notmux-linux-x64.tar.gz`
- `notmux-windows-x64.zip`
- `SHA256SUMS` for those CI artifacts

Branch pushes and manual workflow runs do not publish releases. Do not rerun a
release workflow after publishing a tag; fix a release with a new version.

## 2. Build and notarize macOS locally

The source checkout must be clean and exactly match the release tag. Set the
identity name (or certificate fingerprint) and the existing keychain profile:

```bash
export SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)'
export NOTARY_PROFILE='notmux-notary'

bash scripts/bundle-macos.sh --release --target aarch64-apple-darwin
```

This builds the embedded web client and locked Rust dependencies, signs the app
with hardened runtime and a secure timestamp, submits it to Apple, checks
acceptance, staples the ticket, and verifies the final app with Gatekeeper.
Only then does it create `dist/releases/0.1.0/notmux-macos-arm64.zip`.
No upload, commit, tag or push is performed by the bundle script.

To also offer Intel, install the Rust target and build it separately:

```bash
rustup target add x86_64-apple-darwin
bash scripts/bundle-macos.sh --release --target x86_64-apple-darwin
```

The second ZIP is `notmux-macos-x64.zip`. Only attach architectures that were
successfully built and checked. Test the Intel build on an Intel Mac or under
Rosetta before advertising Intel support.

Release mode intentionally produces **ZIPs**, because curl installation and the
self-updater consume ZIPs. Do not combine it with `--skip-build`, `--dmg` or
`--pkg`. Those options remain available for development bundles, which are
ad-hoc signed and must not be uploaded as public releases.

## 3. Assemble and verify the complete asset set

Set these values to the version being released:

```bash
VERSION=0.1.0
TAG="v$VERSION"
REPO=notagentdev/notmux
DIR="dist/releases/$VERSION"

test "$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq .isDraft)" = true

gh release download "$TAG" --repo "$REPO" --dir "$DIR" \
  --pattern notmux-linux-x64.tar.gz --pattern notmux-windows-x64.zip

gh release download "$TAG" --repo "$REPO" --pattern SHA256SUMS \
  --output "$DIR/CI-SHA256SUMS"

(cd "$DIR" && shasum -a 256 -c CI-SHA256SUMS)
(cd "$DIR" && shasum -a 256 notmux-*.zip notmux-*.tar.gz > SHA256SUMS)
(cd "$DIR" && shasum -a 256 -c SHA256SUMS)
```

Keep the checksum manifest complete: it must cover **all** uploaded installer
archives, not just the macOS ZIP most recently built. Never upload an old ZIP
from another version. The version-scoped directory prevents mixing different
release versions; rebuild and review any rerun of the same version.

## 4. Upload, review, then explicitly publish

```bash
test "$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq .isDraft)" = true

gh release upload "$TAG" --repo "$REPO" \
  "$DIR"/notmux-macos-*.zip "$DIR/SHA256SUMS" --clobber

gh release view "$TAG" --repo "$REPO" --web
```

Review release notes, asset names, checksums and the notarized macOS build.
Perform a first-install smoke check on the intended platforms. Publish only
after that review:

```bash
gh release edit "$TAG" --repo "$REPO" --draft=false --latest
```

Only now can unauthenticated users install the release:

```bash
curl -fsSL https://raw.githubusercontent.com/notagentdev/notmux/main/install.sh | bash
```

To select a published version explicitly:

```bash
curl -fsSL https://raw.githubusercontent.com/notagentdev/notmux/main/install.sh | bash -s -- 0.1.0
```

The macOS installer checks SHA256, the app identifier, signature and Gatekeeper
approval. It installs the complete app into `/Applications/NotMux.app`, preserving
the old installation on a failed replacement. It does not bypass quarantine.
Linux is currently x86_64 only. Windows users use `install.ps1`, which also
requires SHA256 verification.

## Updates and Homebrew

The app checks the latest published release after 30 seconds, then every
24 hours. Download is automatic; installation and restart are user-triggered.
Missing or invalid checksums fail closed. macOS self-updates replace the whole
bundle, require the same signing team and valid Gatekeeper approval, and retain
the old bundle until restart. The new process is launched after the old one exits.

Development/ad-hoc macOS installations must install the first Developer ID
signed release manually via curl; they cannot establish signing-team continuity
for a signed self-update. The ZIP cannot be replaced with just a DMG or binary.

Homebrew installations still skip self-update. Updating `homebrew-notmux` is
manual and optional, not a prerequisite for curl installation. Update the tap's
version and hashes only after publishing the corresponding assets. The current
cask expects both ARM and Intel ZIPs; do not advertise it until both exist.
