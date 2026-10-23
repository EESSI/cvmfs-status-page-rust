"""Smoke-test the actual production image with persistent state and a read-only root."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import ProxyHandler, build_opener
import uuid

ROOT = Path(__file__).resolve().parents[1]


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


def check_runtime(container, root, binary):
    archive = root / "rootfs.tar"
    docker("export", "--output", str(archive), container)
    with tarfile.open(archive) as filesystem:
        members = {member.name.removeprefix("./").lstrip("/"): member for member in filesystem.getmembers()}
        # Container engines may add these files when creating a container.
        engine_files = {"etc/hosts", "etc/hostname", "etc/resolv.conf", ".dockerenv", "run/.containerenv"}
        files = {name for name, member in members.items() if not member.isdir()}
        executable = "usr/local/bin/cvmfs-status-server"
        certificates = "etc/ssl/certs/ca-certificates.crt"
        assert files - engine_files == {executable, certificates}, files
        assert members[executable].mode == 0o755
        assert b"-----BEGIN CERTIFICATE-----" in filesystem.extractfile(members[certificates]).read()
        state = members["var/lib/cvmfs-status"]
        assert (state.uid, state.gid, state.mode) == (10001, 10001, 0o755), state
        assert members["tmp"].mode == 0o1777
        if binary:
            actual = hashlib.sha256(filesystem.extractfile(members[executable]).read()).digest()
            assert actual == hashlib.sha256(binary.read_bytes()).digest(), "Image binary differs from release binary"


def smoke(image, binary=None):
    volume = "cvmfs-status-test-" + uuid.uuid4().hex
    container = None
    config_container = None
    config_volume = volume + "-config"
    opener = build_opener(ProxyHandler({}))
    docker("volume", "create", volume)
    docker("volume", "create", config_volume)
    try:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            root.chmod(0o755)
            cfg = json.loads((ROOT / "config.json").read_text())
            cfg["servers"] = []
            config = root / "config.json"
            config.write_text(json.dumps(cfg))
            config.chmod(0o644)
            # A named read-only config volume also works with rootless engines
            # whose user namespace cannot read a host user's bind-mounted files.
            config_container = docker("create", "--user=0:0", "-v",
                                      f"{config_volume}:/etc/cvmfs-status", image)
            check_runtime(config_container, root, binary)
            docker("cp", str(config), f"{config_container}:/etc/cvmfs-status/config.json")
            docker("rm", config_container)
            config_container = None
            container = docker("run", "-d", "--read-only", "--cap-drop=ALL",
                               "--security-opt=no-new-privileges", "--tmpfs", "/tmp:rw,nosuid,nodev,size=64m",
                               "-v", f"{volume}:/var/lib/cvmfs-status", "-v", f"{config_volume}:/etc/cvmfs-status:ro",
                               "-p", "127.0.0.1::8080", "-p", "127.0.0.1::9090", image,
                               "--configuration=/etc/cvmfs-status/config.json", "--state-directory=/var/lib/cvmfs-status",
                               "--public-address=0.0.0.0:8080", "--operational-address=0.0.0.0:9090")
            for restart in (False, True):
                if restart:
                    docker("kill", "--signal=KILL", container)
                    docker("start", container)
                public = "http://" + docker("port", container, "8080/tcp")
                ops = "http://" + docker("port", container, "9090/tcp")
                deadline = time.monotonic() + 30
                while True:
                    try:
                        with opener.open(ops + "/readyz", timeout=1) as response:
                            assert response.status == 200
                        break
                    except (URLError, TimeoutError, ConnectionError):
                        if time.monotonic() > deadline:
                            raise AssertionError(docker("logs", container))
                        time.sleep(0.1)
                with opener.open(public + "/status.json") as response:
                    data = json.load(response)
                    assert data["eessi_status"]["status"] == "FAILED"
                    assert "<!DOCTYPE html>" in opener.open(public + "/").read().decode()
                try:
                    opener.open(public + "/does-not-exist")
                except HTTPError as response:
                    with response:
                        assert response.code == 404
                        assert response.headers["Content-Type"] == "text/html; charset=utf-8"
                        assert b"Page not found" in response.read()
                else:
                    raise AssertionError("Missing public URL did not return 404")
            docker("stop", "--time=5", container)
            info = json.loads(docker("inspect", container))[0]
            assert info["Config"]["User"] == "10001:10001"
            assert info["HostConfig"]["ReadonlyRootfs"]
            assert info["State"]["ExitCode"] == 0, info["State"]
            print("Production image passed: minimal rootfs, CA bundle, non-root, read-only root/config, persistence, restart and SIGTERM")
    finally:
        if config_container:
            docker("rm", "-f", config_container)
        if container:
            docker("rm", "-f", container)
        docker("volume", "rm", volume)
        docker("volume", "rm", config_volume)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image")
    parser.add_argument("--binary", type=Path, help="Require the image to contain this exact release binary")
    args = parser.parse_args()
    smoke(args.image, args.binary)
