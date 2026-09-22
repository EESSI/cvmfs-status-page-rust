"""Standard-library acceptance tests for generator, frozen reference and HTTP parity."""

import argparse
from contextlib import contextmanager
from dataclasses import replace
from datetime import datetime, timezone
from html import unescape
from http.server import ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from urllib.error import HTTPError, URLError
from urllib.request import ProxyHandler, Request, build_opener

from live_scrape_compare import normalize_json, normalize_metrics
import test_html_output as html_fixture
from test_html_output import FixtureHandler, SCENARIOS

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests/fixtures/compatibility"
GENERATOR = ROOT / "target/debug/cvmfs-status-page-rust"
SERVICE = ROOT / "target/debug/cvmfs-status-server"
OPENER = build_opener(ProxyHandler({}))
SEED_TS = int(time.time()) // 86400 * 86400 - 86400
SCENARIOS = (*SCENARIOS, replace(SCENARIOS[2], name="expired_grace", grace_seconds=1),
             replace(SCENARIOS[1], name="grafana_fallback"),
             replace(SCENARIOS[0], name="custom_template", nested=True))


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def request(base, path, method="GET", headers=None):
    req = Request(f"{base}/{path}", method=method, headers=headers or {})
    try:
        response = OPENER.open(req, timeout=5)
    except HTTPError as error:
        response = error
    with response:
        return response.status, response.headers, response.read()


@contextmanager
def upstream(scenario, delay=0):
    class Handler(FixtureHandler):
        def do_GET(self):
            if delay:
                time.sleep(delay)
            try:
                super().do_GET()
            except (BrokenPipeError, ConnectionResetError):
                pass
    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        server.scenario = scenario
        server.unexpected = []
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        env = {k: v for k, v in os.environ.items() if not k.lower().endswith("_proxy")}
        env.update(http_proxy=f"http://127.0.0.1:{server.server_port}", no_proxy="",
                   HTML_COMPARISON_TOKEN="fixture")
        if scenario.name == "grafana_fallback":
            env.pop("HTML_COMPARISON_TOKEN")
        try:
            yield env
            if server.unexpected:
                raise AssertionError(server.unexpected)
        finally:
            server.shutdown()
            thread.join()


def paths(scenario):
    return ("status/index.html" if scenario.nested else "index.html",
            "capacity/trends.html" if scenario.nested else "trends.html")


def seed(root, scenario):
    root.mkdir(parents=True, exist_ok=True)
    if scenario.name == "expired_grace":
        (root / "replication-state.json").write_text(json.dumps({
            "version": 2, "repositories": {name: {"expired_through": 12, "revisions": {}}
                                              for name in ("alpha.test", "zeta.test")}}))
    if scenario.name == "grafana_fallback":
        history = root / "history"
        history.mkdir()
        # Seed one historical sample at the previous UTC midnight.
        (history / "snapshots.jsonl").write_text(json.dumps({
            "v": 1, "t": SEED_TS, "run_duration_ms": 1, "overall": "OK",
            "categories": {}, "servers": {}, "ext": {"s1_disk_bytes": 1048576}}) + "\n")


def config_file(root, scenario):
    config = html_fixture.HtmlOutputComparison().configuration(scenario)
    path = root / "config.json"
    path.write_text(json.dumps(config))
    return path


def custom(root):
    (root / "templates").mkdir(parents=True, exist_ok=True)
    (root / "templates/_header.html").write_text("<header>Local template {{ title }}</header>")


def run_generator(binary, root, scenario, env):
    destination = root / "output"
    seed(destination, scenario)
    if scenario.name == "custom_template":
        custom(destination)
    status, trends = paths(scenario)
    subprocess.run([str(binary), "--configuration", str(config_file(root, scenario)),
                    "--destination", str(destination), "--output-file", status,
                    "--trends-output-file", trends, "--prometheus-metrics"],
                   env=env, cwd=root, check=True, capture_output=True, timeout=40)
    names = [status, trends, "status.json", "trends.json", "metrics"]
    if scenario.history:
        names.append("history.json")
    return {name: (destination / name).read_bytes() for name in names}


def normalized(artifacts):
    status = json.loads(artifacts["status.json"])
    trends = json.loads(artifacts["trends.json"])
    result = {}
    sample_ts = status.get("history_meta", {}).get("latest_snapshot")
    for name, data in artifacts.items():
        if name.endswith(".html"):
            text = data.decode()
            for timestamp in (status["last_update"], trends["generated_at"]):
                text = text.replace(f"Last updated {timestamp} |", "Last updated <GENERATED_AT> |")
            result[name] = text
        elif name == "metrics":
            result[name] = normalize_metrics(data.decode())
        else:
            value = normalize_json(json.loads(data))
            if name == "status.json":
                for server in value.get("servers_enriched", []):
                    if sample_ts is not None and server.get("uptime", {}).get("last_ok_at") == sample_ts:
                        server["uptime"]["last_ok_at"] = "<CURRENT_SAMPLE>"
            if name == "status.json" and "history_meta" in value:
                # These timestamps describe the newly recorded sample, not upstream facts.
                meta = value["history_meta"]
                meta["latest_snapshot"] = "<CURRENT_SAMPLE>"
                if meta.get("earliest_snapshot") == SEED_TS:
                    meta["earliest_snapshot"] = "<HISTORICAL_SAMPLE>"
                if meta["snapshots_raw"] == 1:
                    meta["earliest_snapshot"] = "<CURRENT_SAMPLE>"
            if name == "trends.json" and "external_metrics" in value:
                ext = value["external_metrics"]
                if sample_ts is not None:
                    if ext.get("sampled_at") == sample_ts:
                        ext["sampled_at"] = "<CURRENT_SAMPLE>"
                    for point in ext["stratum1_disk_usage"]["series"]:
                        if point["t"] == sample_ts:
                            point["t"] = "<CURRENT_SAMPLE>"
                if ext["fallback_from_history"]:
                    if ext.get("sampled_at") == SEED_TS:
                        ext["sampled_at"] = "<HISTORICAL_SAMPLE>"
                    for point in ext["stratum1_disk_usage"]["series"]:
                        if point["t"] == SEED_TS:
                            point["t"] = "<HISTORICAL_SAMPLE>"
                if "fetched_at" in ext:
                    ext["fetched_at"] = "<FETCH_TIME>"
            if name == "history.json":
                # Calendar buckets are relative to the evaluation date; keep offsets,
                # values and source revision timestamps intact.
                today = datetime.fromtimestamp(json.loads(data)["generated_at"], timezone.utc).date()
                for repo in value["repositories"].values():
                    for point in repo["revision_series"]:
                        if sample_ts is not None and point["t"] == sample_ts:
                            point["t"] = "<CURRENT_SAMPLE>"
                for server in value["servers"].values():
                    if sample_ts is not None and server["uptime"].get("last_ok_at") == sample_ts:
                        server["uptime"]["last_ok_at"] = "<CURRENT_SAMPLE>"
                    for bar in server["bars"]:
                        bar["d"] = f"day:{(datetime.fromisoformat(bar['d']).date() - today).days}"
                    for incident in server["incidents_90d"]:
                        for field in ("start", "end"):
                            if incident.get(field) == json.loads(data)["generated_at"]:
                                incident[field] = "<CURRENT_SAMPLE>"
            result[name] = value
    return result


class RunningService:
    def __init__(self, root, scenario, env, *, interval=86400, deadline=5, extra=()):
        self.root = root
        self.public = f"http://127.0.0.1:{port()}"
        self.ops = f"http://127.0.0.1:{port()}"
        status, trends = paths(scenario)
        self.log = (root / "service.log").open("wb")
        args = [str(SERVICE), "--configuration", str(root / "config.json"),
                "--state-directory", str(root / "state"), "--public-address", self.public[7:],
                "--operational-address", self.ops[7:], "--interval-seconds", str(interval),
                "--collection-deadline-seconds", str(deadline), "--shutdown-grace-seconds", "2",
                "--output-file", status, "--trends-output-file", trends, "--prometheus-metrics"]
        if scenario.name == "custom_template":
            args += ["--override-directory", str(root / "overrides")]
        self.process = subprocess.Popen(args + list(extra), cwd=root, env=env,
                                        stdout=self.log, stderr=subprocess.STDOUT)

    def wait(self, predicate=None):
        limit = time.monotonic() + 20
        while time.monotonic() < limit:
            if self.process.poll() is not None:
                raise AssertionError((self.root / "service.log").read_text())
            try:
                response = request(self.ops, "diagnostics")
                diagnostics = json.loads(response[2])
                if predicate(diagnostics) if predicate else diagnostics["ready"]:
                    return diagnostics
            except (URLError, TimeoutError, ConnectionError):
                pass
            time.sleep(0.03)
        raise AssertionError("service not ready: " + (self.root / "service.log").read_text())

    def stop(self, force=False):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGKILL if force else signal.SIGTERM)
            self.process.wait(timeout=8)
        self.log.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.stop()


class ServiceAcceptance(unittest.TestCase):
    def test_unknown_public_url_displays_error(self):
        scenario = SCENARIOS[0]
        with tempfile.TemporaryDirectory() as tmp, upstream(scenario) as env:
            root = Path(tmp)
            config_file(root, scenario)
            with RunningService(root, scenario, env) as service:
                service.wait()
                status, headers, body = request(service.public, "does-not-exist")
                self.assertEqual(status, 404)
                self.assertEqual(headers["Content-Type"], "text/html; charset=utf-8")
                self.assertIn(b"Page not found", body)
                self.assertIn('href="/index.html"', unescape(body.decode()))
                length = len(body)
                status, headers, body = request(service.public, "does-not-exist", "HEAD")
                self.assertEqual(status, 404)
                self.assertEqual(int(headers["Content-Length"]), length)
                self.assertEqual(body, b"")

    def test_custom_not_found_template_is_loaded_at_startup(self):
        scenario = replace(SCENARIOS[0], nested=True)
        with tempfile.TemporaryDirectory() as tmp, upstream(scenario) as env:
            root = Path(tmp)
            config_file(root, scenario)
            overrides = root / "overrides"
            (overrides / "templates").mkdir(parents=True)
            template = overrides / "templates/404.html"
            template.write_text('<h1>Custom missing page</h1><a href="{{ home_url }}">Return</a>')
            extra = ("--override-directory", str(overrides))
            with RunningService(root, scenario, env, extra=extra) as service:
                service.wait()
                status, _, original = request(service.public, "nested/unknown")
                self.assertEqual(status, 404)
                self.assertEqual(unescape(original.decode()),
                                 '<h1>Custom missing page</h1><a href="/status/index.html">Return</a>')
                self.assertEqual(request(service.public, "404.html")[2], original)
                template.write_text('<h1>Updated missing page</h1>')
                self.assertEqual(request(service.public, "nested/unknown")[2], original)
            with RunningService(root, scenario, env, extra=extra) as restarted:
                restarted.wait()
                status, _, body = request(restarted.public, "nested/unknown")
                self.assertEqual(status, 404)
                self.assertEqual(body, b'<h1>Updated missing page</h1>')

    def test_reference_generator_http_parity(self):
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario.name), tempfile.TemporaryDirectory() as tmp, upstream(scenario) as env:
                root = Path(tmp)
                generated = run_generator(GENERATOR, root, scenario, env)
                expected = json.loads((FIXTURES / f"{scenario.name}.json").read_text())
                self.assertEqual(expected, normalized(generated))
                seed(root / "state", scenario)
                if scenario.name == "custom_template":
                    custom(root / "overrides")
                with RunningService(root, scenario, env) as service:
                    service.wait()
                    served = {}
                    for name, body in generated.items():
                        status, headers, actual = request(service.public, name)
                        self.assertEqual(status, 200, name)
                        served[name] = actual
                        head = request(service.public, name, "HEAD")
                        self.assertEqual(head[0], 200)
                        self.assertEqual(head[2], b"")
                        self.assertEqual(int(head[1]["Content-Length"]), len(actual))
                        self.assertEqual(request(service.public, name, headers={"If-None-Match": headers["ETag"]})[0], 304)
                    self.assertEqual(expected, normalized(served))
                    self.assertEqual(request(service.public, "status.css")[2], (root / "output/status.css").read_bytes())
                    self.assertEqual(request(service.public, "")[0], 404 if scenario.nested else 200)
                    for private in ("config.json", "service.json", "templates/status.html", "history/snapshots.jsonl", "replication-state.json", "committed.json", "diagnostics", "readyz", "generations", "../config.json"):
                        self.assertEqual(request(service.public, private)[0], 404, private)
                    self.assertEqual(request(service.public, "status.json", "POST")[0], 405)
                    self.assertNotIn("settings", json.loads(served["status.json"])["config"])
                    self.assertTrue(request(service.ops, "metrics")[2].startswith(b"# TYPE cvmfs_service_"))

    def test_cold_start_and_deadline_publish_fresh_upstream_failure(self):
        scenario = SCENARIOS[0]
        with tempfile.TemporaryDirectory() as tmp, upstream(scenario, delay=2) as env:
            root = Path(tmp)
            config_file(root, scenario)
            with RunningService(root, scenario, env, deadline=1) as service:
                report = service.wait(lambda d: d["runtime"]["attempts"] == 1)
                self.assertFalse(report["ready"])
                self.assertEqual(request(service.public, "status.json")[0], 503)
                status, _, body = request(service.public, "does-not-exist")
                self.assertEqual(status, 404)
                self.assertIn(b"Page not found", body)
                report = service.wait()
                self.assertFalse(report["freshness_degraded"])
                status = json.loads(request(service.public, "status.json")[2])
                self.assertEqual(status["eessi_status"]["status"], "FAILED")
                self.assertTrue(all(server["status"] == "FAILED" for server in status["servers"]))

    def test_failed_commit_keeps_bundle_and_recovers_after_kill(self):
        scenario = SCENARIOS[0]
        with tempfile.TemporaryDirectory() as tmp, upstream(scenario) as env:
            root = Path(tmp)
            config_file(root, scenario)
            seed(root / "state", scenario)
            service = RunningService(root, scenario, env, interval=1)
            try:
                service.wait()
                first = request(service.public, "status.json")[2]
                # A directory in place of the backup pointer prevents future commits.
                backup = root / "state/previous-committed.json"
                backup.unlink()
                backup.mkdir()
                report = service.wait(lambda d: d["runtime"]["failures"] > 0)
                self.assertTrue(report["ready"])
                self.assertEqual(request(service.public, "status.json")[2], first)
                service.stop(force=True)
                # Cached timestamps survive a restart even with upstream blocked.
                with RunningService(root, scenario, env) as restarted:
                    restarted.wait()
                    self.assertEqual(request(restarted.public, "status.json")[2], first)
            finally:
                service.stop()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-fixtures", type=Path, help="Freeze outputs from the pre-extraction reference binary")
    parser.add_argument("--generator", type=Path, default=GENERATOR)
    parser.add_argument("--service", type=Path, default=SERVICE)
    args, remaining = parser.parse_known_args()
    GENERATOR, SERVICE = args.generator.resolve(), args.service.resolve()
    if args.write_fixtures:
        FIXTURES.mkdir(parents=True, exist_ok=True)
        for scenario in SCENARIOS:
            with tempfile.TemporaryDirectory() as tmp, upstream(scenario) as env:
                data = run_generator(args.write_fixtures.resolve(), Path(tmp), scenario, env)
                (FIXTURES / f"{scenario.name}.json").write_text(json.dumps(normalized(data), indent=2, sort_keys=True) + "\n")
    else:
        unittest.main(argv=[__file__, *remaining])
