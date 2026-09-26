"""Regression checks for workspace boundaries and production build inputs."""
import fnmatch
import json
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


class BuildContext(unittest.TestCase):
    def test_reference_checkouts_are_independent_cargo_workspaces(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "Cargo.toml").write_text((ROOT / "Cargo.toml").read_text())

            def package(path, name):
                (path / "src").mkdir(parents=True)
                (path / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\nversion = "0.0.0"\n'
                )
                (path / "src/lib.rs").write_text("")

            package(root / "crates/probe", "workspace-library")
            package(root / "apps/cvmfs-status-page-rust", "workspace-app")
            for workflow, directory in (
                ("ci.yml", ".html-reference"),
                ("live-scrape.yml", ".live-reference"),
            ):
                with self.subTest(directory=directory):
                    self.assertIn(
                        f"path: {directory}",
                        (ROOT / ".github/workflows" / workflow).read_text(),
                    )
                    reference = root / directory
                    package(reference, "reference")
                    result = subprocess.run(
                        ["cargo", "metadata", "--offline", "--no-deps", "--format-version=1"],
                        cwd=reference, capture_output=True, text=True, timeout=30,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    metadata = json.loads(result.stdout)
                    self.assertEqual(Path(metadata["workspace_root"]), reference)
                    self.assertEqual(len(metadata["workspace_members"]), 1)

    def test_every_manifest_and_embedded_input_is_copied(self):
        dockerfile = (ROOT / "Dockerfile").read_text()
        copies = re.findall(r"^COPY (?!-)(.+) \\?\S+$", dockerfile, re.MULTILINE)
        copied = {part for group in copies for part in group.split()}
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
        members = [p for pattern in workspace["workspace"]["members"] for p in ROOT.glob(pattern)]
        self.assertEqual(len(members), 8)
        for member in members:
            self.assertIn(member.relative_to(ROOT).parts[0], copied)
            self.assertTrue((member / "Cargo.toml").is_file())
        for name in ["Cargo.toml", "Cargo.lock", "config.json"]:
            self.assertIn(name, copied)
        embedded_inputs = set()
        for source in ROOT.glob("crates/*/src/*.rs"):
            text = source.read_text()
            for path in re.findall(r'include_(?:str|bytes)!\("([^"$]+)"\)', text):
                target = (source.parent / path).resolve()
                self.assertTrue(target.exists(), target)
                self.assertIn(target.relative_to(ROOT).parts[0], copied)
                embedded_inputs.add(target)
            for path in re.findall(r'include_dir!\("\$CARGO_MANIFEST_DIR/([^"\n]+)"\)', text):
                directory = source.parent.parent / path
                self.assertTrue(directory.is_dir(), directory)
                embedded_inputs.update(p for p in directory.rglob("*") if p.is_file())
        self.assertIn(ROOT / "crates/status-presentation/templates/404.html", embedded_inputs)
        rules = (ROOT / ".dockerignore").read_text().splitlines()
        for target in [member / "Cargo.toml" for member in members] + sorted(embedded_inputs):
            rel = target.relative_to(ROOT).as_posix()
            self.assertIn(target.relative_to(ROOT).parts[0], copied)
            included = False
            for rule in rules:
                if fnmatch.fnmatch(rel, rule.lstrip("!")):
                    included = rule.startswith("!")
            self.assertTrue(included, rel)
        self.assertIn('USER 10001:10001', dockerfile)

    def test_dependency_arrows_and_private_packages(self):
        forbidden = {
            "status-domain": {"status-application", "status-storage", "status-storage-fs", "cvmfs_server_scraper", "actix-web"},
            "status-storage": {"status-application", "status-storage-fs", "actix-web"},
            "status-application": {"status-storage-fs", "status-presentation", "status-sources", "actix-web"},
            "status-storage-fs": {"status-application", "status-presentation", "actix-web"},
        }
        for manifest in list(ROOT.glob("crates/*/Cargo.toml")) + list(ROOT.glob("apps/*/Cargo.toml")):
            data = tomllib.loads(manifest.read_text())
            self.assertEqual(data["package"]["publish"], {"workspace": True})
            self.assertFalse(forbidden.get(data["package"]["name"], set()) & data.get("dependencies", {}).keys())
        release = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertNotIn(".packages[0]", release)
        for name in ("ci.yml", "release.yml"):
            workflow = (ROOT / ".github/workflows" / name).read_text()
            self.assertIn("test_container.py", workflow)
            self.assertIn("docker build", workflow)
            self.assertNotIn("paths-ignore:", workflow)


if __name__ == "__main__":
    unittest.main()
