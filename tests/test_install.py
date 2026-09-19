"""Exercise the installer with local release archives and no network access."""

import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().parents[1] / "scripts" / "install.sh"
BINARY = "cvmfs-status-page-rust"
VERSION = "0.0.1"


class InstallerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="installer-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.install_dir = self.root / "install directory"
        self.install_dir.mkdir()
        self.destination = self.install_dir / BINARY
        self.downloads = self.root / "downloads"
        self.downloads.mkdir()
        shims = self.root / "shims"
        shims.mkdir()
        self.env = dict(
            os.environ,
            PATH=f"{shims}:{os.environ['PATH']}",
            TMPDIR=str(self.downloads),
            TEST_ASSETS=str(self.assets),
            TEST_ARCH="x86_64",
            TEST_DOWNLOAD_FAIL="0",
        )
        # Stub only network access and architecture detection. The installer
        # uses the real checksum, extraction, staging, and filesystem commands.
        commands = {
            "curl": """#!/bin/sh
set -eu
[ "$TEST_DOWNLOAD_FAIL" = 0 ] || exit 22
[ "$#" -eq 4 ] && [ "$1" = -fsSL ] && [ "$3" = -o ] || exit 2
cp "$TEST_ASSETS/${2##*/}" "$4"
""",
            "uname": """#!/bin/sh
case "$1" in
    -s) echo Linux ;;
    -m) echo "$TEST_ARCH" ;;
    *) exit 2 ;;
esac
""",
        }
        for name, content in commands.items():
            command = shims / name
            command.write_text(content)
            command.chmod(0o755)

    def make_release(self, reported_version=VERSION, exit_status=0):
        package = f"{BINARY}-{VERSION}-{self.env['TEST_ARCH']}-unknown-linux-gnu"
        binary = (
            "#!/bin/sh\n"
            '[ "$#" -eq 1 ] && [ "$1" = --version ] || exit 2\n'
            f"echo '{BINARY} {reported_version}'\n"
            + ("echo 'version probe failed' >&2\n" if exit_status else "")
            + f"exit {exit_status}\n"
        ).encode()
        archive = self.assets / f"{package}.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            member = tarfile.TarInfo(f"{package}/{BINARY}")
            member.mode = 0o755
            member.size = len(binary)
            output.addfile(member, io.BytesIO(binary))
        checksum = self.assets / f"{archive.name}.sha256"
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        checksum.write_text(f"{digest}  {archive.name}\n")
        return archive, checksum

    def run_installer(self, *release_args):
        result = subprocess.run(
            [
                "sh",
                str(INSTALLER),
                *(release_args or ("--version", VERSION)),
                "--install-dir",
                str(self.install_dir),
            ],
            env=self.env,
            capture_output=True,
            text=True,
            timeout=10,
        )
        self.assertEqual(list(self.downloads.iterdir()), [], "downloads not cleaned")
        self.assertEqual(
            list(self.install_dir.glob(f".{BINARY}.tmp.*")),
            [],
            "staged binary not cleaned",
        )
        return result

    def assert_installed(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.destination.is_file())
        self.assertFalse(self.destination.is_symlink())
        self.assertEqual(self.destination.stat().st_mode & 0o777, 0o755)
        self.assertEqual(
            subprocess.check_output([str(self.destination), "--version"], text=True),
            f"{BINARY} {VERSION}\n",
        )

    def existing_binary(self):
        self.destination.write_text("#!/bin/sh\necho 'working old binary'\n")
        self.destination.chmod(0o755)
        return self.destination.read_bytes()

    def assert_preserved(self, result, original):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("Installed ", result.stdout)
        self.assertEqual(self.destination.read_bytes(), original)
        self.assertEqual(self.destination.stat().st_mode & 0o777, 0o755)

    def test_install_by_version(self):
        self.make_release()
        self.assert_installed(self.run_installer())

    def test_install_by_tag(self):
        self.make_release()
        self.assert_installed(self.run_installer("--tag", f"v{VERSION}"))

    def test_arm64_asset_selection(self):
        self.env["TEST_ARCH"] = "aarch64"
        self.make_release()
        self.assert_installed(self.run_installer())

    def test_upgrade_existing_binary(self):
        self.existing_binary()
        self.make_release()
        self.assert_installed(self.run_installer())

    def test_download_failure_preserves_existing_binary(self):
        original = self.existing_binary()
        self.env["TEST_DOWNLOAD_FAIL"] = "1"
        self.assert_preserved(self.run_installer(), original)

    def test_bad_checksum_preserves_existing_binary(self):
        original = self.existing_binary()
        archive, checksum = self.make_release()
        checksum.write_text(f"{'0' * 64}  {archive.name}\n")
        self.assert_preserved(self.run_installer(), original)

    def test_wrong_version_preserves_existing_binary(self):
        original = self.existing_binary()
        self.make_release(reported_version="0.0.2")
        self.assert_preserved(self.run_installer(), original)

    def test_failed_version_probe_preserves_binary_and_stderr(self):
        original = self.existing_binary()
        self.make_release(exit_status=1)
        result = self.run_installer()
        self.assert_preserved(result, original)
        self.assertIn("version probe failed", result.stderr)

    def test_destination_directory_is_rejected(self):
        self.destination.mkdir()
        self.make_release()
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("Installed ", result.stdout)
        self.assertEqual(list(self.destination.iterdir()), [])

    def test_destination_symlink_is_replaced_without_modifying_directory(self):
        target = self.root / "symlink target"
        target.mkdir()
        self.destination.symlink_to(target, target_is_directory=True)
        self.make_release()
        self.assert_installed(self.run_installer())
        self.assertEqual(list(target.iterdir()), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
