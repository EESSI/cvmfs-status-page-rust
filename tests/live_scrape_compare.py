"""Capture public CVMFS HTTP responses once and compare two binaries by replay."""

import argparse
import base64
from contextlib import contextmanager
from datetime import datetime
import difflib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import build_opener, HTTPRedirectHandler, ProxyHandler


STATUSES = {"OK", "DEGRADED", "WARNING", "FAILED", "MAINTENANCE"}
CASSETTE_VERSION = 2
UNORDERED_ARRAYS = {
    ("servers",), ("servers_enriched",), ("repositories",), ("repositories_enriched",),
    ("servers_enriched", "repositories"), ("servers", "replication_details"),
    ("summary", "geographic_spread", "stratum1_countries"),
} | {
    path
    for group in ("stratum0", "stratum1", "syncservers")
    for path in ((group, "servers"), (group, "details"), (group, "servers", "replication_details"))
}


def request_key(url):
    parsed = urlsplit(url)
    if parsed.scheme != "http" or not parsed.netloc or parsed.username or parsed.password:
        raise ValueError("Only public HTTP scrape URLs are supported")
    parts = parsed.path.split("/")
    if parts[3:6] == ["api", "v1.0", "geo"] and len(parts) == 8:
        parts[6] = "<nonce>"
    return urlunsplit((parsed.scheme, parsed.netloc, "/".join(parts), parsed.query, ""))


class PreserveRedirects(HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        # Each binary must follow the original redirect through the proxy itself.
        return None


class Cassette:
    def __init__(self, hosts, responses=None):
        self.hosts = set(hosts)
        self.responses = {} if responses is None else responses
        self.replay = responses is not None
        self.requested = set()
        self.errors = []
        self.lock = threading.Lock()
        self.key_locks = {}
        self.opener = build_opener(ProxyHandler({}), PreserveRedirects())

    def response(self, url):
        key = request_key(url)
        if urlsplit(url).netloc not in self.hosts:
            raise ValueError(f"Unexpected scrape host: {urlsplit(url).netloc}")
        with self.lock:
            self.requested.add(key)
            key_lock = self.key_locks.setdefault(key, threading.Lock())
        with key_lock:
            if key in self.responses:
                return self.responses[key]
            if self.replay:
                raise ValueError(f"Unrecorded request during replay: {key}")
            try:
                try:
                    upstream = self.opener.open(url, timeout=15)
                except HTTPError as error:
                    upstream = error
                with upstream:
                    result = {
                        "status": upstream.status,
                        "content_type": upstream.headers.get("Content-Type", "application/octet-stream"),
                        "body": base64.b64encode(upstream.read()).decode("ascii"),
                    }
                    if (location := upstream.headers.get("Location")) is not None:
                        result["location"] = location
            except (URLError, TimeoutError, OSError) as error:
                # A recorded transport failure remains identical for both binaries.
                result = {
                    "status": 502, "content_type": "text/plain",
                    "body": base64.b64encode(b"Recorded upstream transport failure").decode("ascii"),
                    "transport_error": str(error),
                }
            self.responses[key] = result
            return result

    def begin_replay(self):
        self.replay = True
        self.requested = set()
        self.errors = []

    def save(self, path):
        path.write_text(json.dumps({"v": CASSETTE_VERSION, "hosts": sorted(self.hosts), "responses": self.responses}, indent=2, sort_keys=True))

    @classmethod
    def load(cls, path, hosts):
        data = json.loads(path.read_text())
        if data["v"] != CASSETTE_VERSION:
            raise ValueError("Cassette version is unsupported; record a fresh capture to preserve redirects")
        if set(data["hosts"]) != set(hosts) or not data["responses"]:
            raise ValueError("Cassette hosts or responses do not match")
        return cls(hosts, data["responses"])


class ReplayHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        try:
            response = self.server.cassette.response(self.path)
            body = base64.b64decode(response["body"], validate=True)
            self.send_response(response["status"])
            self.send_header("Content-Type", response["content_type"])
            if "location" in response:
                self.send_header("Location", response["location"])
        except Exception as error:
            self.server.cassette.errors.append(str(error))
            body = str(error).encode()
            self.send_response(502)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_CONNECT(self):
        self.server.cassette.errors.append("HTTPS proxy requests are not supported")
        self.send_error(405)

    def do_POST(self):
        self.server.cassette.errors.append("Only GET requests are supported")
        self.send_error(405)

    def log_message(self, *_args):
        pass


@contextmanager
def proxy_for(cassette):
    with ThreadingHTTPServer(("127.0.0.1", 0), ReplayHandler) as server:
        server.cassette = cassette
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}"
        finally:
            server.shutdown()
            thread.join()


def normalize_json(value, path=()):
    if path in {
        ("last_update",), ("generated_at",),
        ("servers_enriched", "repositories", "replication_grace", "first_observed_at"),
    }:
        if path == ("last_update",):
            datetime.fromisoformat(value.replace("Z", "+00:00"))
        elif type(value) is not int:
            raise ValueError(f"Invalid generated timestamp: {path}")
        return "<GENERATED_AT>"
    if isinstance(value, dict):
        return {name: normalize_json(item, path + (name,)) for name, item in value.items()}
    if isinstance(value, list):
        result = [normalize_json(item, path) for item in value]
        if path in UNORDERED_ARRAYS:
            result.sort(key=lambda item: json.dumps(item, sort_keys=True))
        return result
    return value


def normalize_metrics(text):
    lines = []
    families = set()
    for line in text.splitlines():
        if not line or line.startswith("#"):
            lines.append(line)
            continue
        sample, timestamp = line.rsplit(" ", 1)
        name, value = sample.rsplit(" ", 1)
        int(timestamp)
        float(value)
        families.add(name.split("{", 1)[0])
        lines.append(sample + " <GENERATED_AT>")
    if not {"eessi_status", "repositories_status"}.issubset(families):
        raise ValueError("Required status metrics are missing")
    return "\n".join(sorted(lines)) + "\n"


def reject_nonfinite(value):
    raise ValueError(f"Invalid JSON number: {value}")


def read_outputs(output):
    status = json.loads((output / "status.json").read_text(), parse_constant=reject_nonfinite)
    trends = json.loads((output / "trends.json").read_text(), parse_constant=reject_nonfinite)
    for group in ["eessi_status", "stratum0", "stratum1", "syncservers", "repositories_status"]:
        if status[group]["status"] not in STATUSES:
            raise ValueError(f"Invalid status for {group}")
    for server in status["servers"]:
        if not isinstance(server["name"], str) or server["status"] not in STATUSES:
            raise ValueError("Invalid server status")
    if not isinstance(status["servers_enriched"], list) or trends["v"] != 1:
        raise ValueError("Invalid status or trends payload")
    outputs = {
        "status.json": json.dumps(normalize_json(status), sort_keys=True, indent=2) + "\n",
        "trends.json": json.dumps(normalize_json(trends), sort_keys=True, indent=2) + "\n",
        "metrics": normalize_metrics((output / "metrics").read_text()),
    }
    return status, outputs


def run_binary(binary, source, output, config_path, proxy, log_path):
    env = {key: value for key, value in os.environ.items() if not key.lower().endswith("_proxy")}
    env.update(http_proxy=proxy, https_proxy=proxy, no_proxy="")
    result = subprocess.run(
        [str(binary), "--configuration", str(config_path), "--destination", str(output), "--prometheus-metrics"],
        cwd=source, env=env, capture_output=True, text=True, timeout=300,
    )
    log_path.write_text(result.stdout + result.stderr)
    if result.returncode:
        raise ValueError(f"{binary.name} exited with {result.returncode}; see {log_path.name}")
    return read_outputs(output)


def compare(args):
    report = args.report_dir.resolve()
    report.mkdir(parents=True, exist_ok=True)
    config = json.loads(args.configuration.read_text())
    # Compare current status with fresh state; public live CI needs no credentials.
    config["history"] = {"enabled": False}
    config["external_metrics"] = None
    config_path = report / "config.json"
    if args.cassette_in:
        captured_config = args.cassette_in.parent / "config.json"
        if json.loads(captured_config.read_text()) != config:
            raise ValueError("Reviewed capture used a different configuration")
    config_path.write_text(json.dumps(config, indent=2))
    hosts = [server["hostname"] for server in config["servers"]]
    cassette = Cassette.load(args.cassette_in, hosts) if args.cassette_in else Cassette(hosts)
    try:
        with tempfile.TemporaryDirectory() as tmp, proxy_for(cassette) as proxy:
            root = Path(tmp)
            status, expected = run_binary(
                args.reference_binary, args.reference_root, root / "reference", config_path,
                proxy, report / "reference.log",
            )
            if cassette.errors:
                raise ValueError("; ".join(cassette.errors))
            if not any(server["repositories"] for server in status["servers_enriched"]):
                raise ValueError("The reference scraped no repositories; refusing an all-error comparison")
            reference_requests = cassette.requested.copy()
            cassette.begin_replay()
            _, actual = run_binary(
                args.candidate_binary, args.candidate_root, root / "candidate", config_path,
                proxy, report / "candidate.log",
            )
            if cassette.errors:
                raise ValueError("; ".join(cassette.errors))
            if cassette.requested != reference_requests:
                raise ValueError("The two binaries did not consume the same recorded requests")
    finally:
        cassette.save(report / "cassette.json")
    differences = []
    for name in expected:
        for kind, outputs in [("reference", expected), ("candidate", actual)]:
            destination = report / kind
            destination.mkdir(exist_ok=True)
            (destination / name).write_text(outputs[name])
        differences.extend(difflib.unified_diff(
            expected[name].splitlines(keepends=True), actual[name].splitlines(keepends=True),
            fromfile=f"main/{name}", tofile=f"candidate/{name}",
        ))
    (report / "diff.txt").write_text("".join(differences))
    summary = {
        "different": bool(differences), "approved": bool(differences) and args.allow_divergence,
        "requests": len(reference_requests), "replayed_capture": args.cassette_in is not None,
    }
    (report / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary))
    return 2 if differences and not args.allow_divergence else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["reference-root", "reference-binary", "candidate-root", "candidate-binary", "configuration", "report-dir"]:
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--cassette-in", type=Path)
    parser.add_argument("--allow-divergence", action="store_true")
    args = parser.parse_args()
    for name in ["reference_root", "reference_binary", "candidate_root", "candidate_binary", "configuration"]:
        setattr(args, name, getattr(args, name).resolve(strict=True))
    try:
        return compare(args)
    except (ValueError, KeyError, TypeError, OSError, subprocess.TimeoutExpired) as error:
        args.report_dir.mkdir(parents=True, exist_ok=True)
        (args.report_dir / "error.txt").write_text(str(error) + "\n")
        print(f"Comparison could not complete: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
