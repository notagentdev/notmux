---
description: Prepare and publish a new release
allowed-tools: Bash(git:*), Bash(cargo check), Read, Edit
---

## Release Process

1. **Find the last release tag and show changes since then:**

!git describe --tags --abbrev=0

!git log $(git describe --tags --abbrev=0)..HEAD --oneline

2. **Analyze the changes above and determine the version bump type:**
   - **MAJOR** (x.0.0): Breaking changes, incompatible API changes
   - **MINOR** (0.x.0): New features, functionality additions (backwards compatible)
   - **PATCH** (0.0.x): Bug fixes, small improvements, refactoring

3. **Read current version from Cargo.toml** and calculate the new version.

4. **Update version in Cargo.toml**.

5. **Run `cargo check` to update Cargo.lock** with the new version.

6. **Commit, push, and create the tag:**
   ```
   git add Cargo.toml Cargo.lock
   git commit -m "chore: bump version to <new-version>"
   git push
   git tag v<new-version>
   git push origin v<new-version>
   ```

7. **Report success** with link to GitHub Actions where the build is running.

## Notes

- CI builds Linux and Windows and creates a draft release; Mobile is disabled.
- Build, sign and notarize macOS locally using `scripts/bundle-macos.sh --release`.
- Follow `docs/releasing.md` to combine artifacts, regenerate SHA256SUMS, and publish explicitly.
- Do not publish the draft before adding and verifying the signed macOS ZIPs.
- Homebrew tap updates are manual and do not block the release.
