"""Offline smoke tests for release installer guardrails; never install a real app."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="notmux-installer-test-")
        self.addCleanup(self.tmp.cleanup)
        self.base = Path(self.tmp.name)
        self.home = self.base / "home"
        self.binary = self.home / ".local/bin/notmux"
        self.binary.parent.mkdir(parents=True)
        self.binary.write_text("old installation")
        self.archive = self.base / "release.tar.gz"
        with tarfile.open(self.archive, "w:gz") as archive:
            data = b"new installation"
            item = tarfile.TarInfo("notmux")
            item.mode = 0o755
            item.size = len(data)
            archive.addfile(item, io.BytesIO(data))
        self.sums = self.base / "SHA256SUMS"
        self.sums.write_text(
            hashlib.sha256(self.archive.read_bytes()).hexdigest()
            + "  notmux-linux-x64.tar.gz\n"
        )
        self.bin = self.base / "bin"
        self.bin.mkdir()
        self.command("uname", 'if [[ "${1:-}" == -s ]]; then echo Linux; else echo "${TEST_ARCH:-x86_64}"; fi')
        self.command("curl", '''
printf '%s\n' "$*" >> "$TEST_CALLS"
case "$*" in
  *api.github.com*)
    [[ "${TEST_FAILURE:-}" != latest ]] || exit 22
    printf '{ "tag_name": "v0.1.0" }\n' ;;
  */SHA256SUMS*)
    [[ "${TEST_FAILURE:-}" != manifest ]] || exit 22
    cp "$TEST_SUMS" "${!#}" ;;
  *.tar.gz*)
    [[ "${TEST_FAILURE:-}" != archive ]] || exit 22
    cp "$TEST_ARCHIVE" "${!#}" ;;
  *) exit 22 ;;
esac
''')
        # Keep optional system integration confined to these fixtures.
        self.command("gtk-update-icon-cache", "exit 0")
        self.command("update-desktop-database", "exit 0")
        self.env = {
            **os.environ,
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
            "HOME": str(self.home),
            "TMPDIR": str(self.base),
            "TEST_ARCHIVE": str(self.archive),
            "TEST_SUMS": str(self.sums),
            "TEST_CALLS": str(self.base / "calls"),
        }

    def command(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/bash\nset -e\n" + body + "\n")
        path.chmod(0o755)

    def run_installer(self, *args, **env):
        return subprocess.run(
            ["bash", str(ROOT / "install.sh"), *args],
            env={**self.env, **env}, capture_output=True, text=True, timeout=10,
        )

    def test_latest_release_installs_only_after_checksum_verification(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.binary.read_text(), "new installation")

    def test_wrong_or_missing_checksum_preserves_previous_installation(self):
        for content in ["0" * 64 + "  notmux-linux-x64.tar.gz\n", ""]:
            with self.subTest(content=content):
                self.sums.write_text(content)
                result = self.run_installer("0.1.0")
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.binary.read_text(), "old installation")

    def test_http_failures_preserve_previous_installation(self):
        for failure in ["latest", "manifest", "archive"]:
            with self.subTest(failure=failure):
                result = self.run_installer(TEST_FAILURE=failure)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.binary.read_text(), "old installation")

    def test_linux_arm_is_rejected_before_download(self):
        result = self.run_installer("0.1.0", TEST_ARCH="aarch64")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.base / "calls").exists())
        self.assertEqual(self.binary.read_text(), "old installation")

    def test_release_mode_requires_explicit_signing_identity(self):
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/bundle-macos.sh"), "--release",
             "--target", "aarch64-apple-darwin"],
            env={**self.env, "SIGNING_IDENTITY": "", "NOTARY_PROFILE": ""},
            capture_output=True, text=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Set SIGNING_IDENTITY", result.stderr)


if __name__ == "__main__":
    unittest.main()
