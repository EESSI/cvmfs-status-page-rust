"""Compare two real generator binaries against identical local HTTP fixtures."""

import argparse
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
REPOSITORIES = ["zeta.test", "alpha.test"]
SERVERS = {
    "s0.test": "Stratum0",
    "zulu.s1.test": "Stratum1",
    "alpha.s1.test": "Stratum1",
    "zulu.sync.test": "SyncServer",
    "alpha.sync.test": "SyncServer",
}


@dataclass(frozen=True)
class Scenario:
    name: str
    lag: int = 0
    grace_seconds: int = 600
    history: bool = False
    external: bool = False
    nested: bool = False
    unavailable: bool = False


SCENARIOS = (
    Scenario("healthy"),
    Scenario("catching_up", lag=2, history=True, external=True, nested=True),
    Scenario("behind_without_grace", lag=2, grace_seconds=0),
    Scenario("unavailable", unavailable=True),
)


class FixtureHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        url = urlsplit(self.path)
        status, body = self.fixture_response(url.hostname, url.path)
        if not isinstance(body, str):
            body = json.dumps(body)
        content = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        self.wfile.write(content)

    def fixture_response(self, host, path):
        scenario = self.server.scenario
        if host == "metrics.test" and path == (
            "/api/datasources/proxy/uid/fixture/api/v1/query_range"
        ):
            return 200, {
                "status": "success",
                "data": {
                    "resultType": "matrix",
                    "result": [{"metric": {}, "values": [[1781740800, "1048576"]]}],
                },
            }
        if host not in SERVERS:
            return self.unexpected_request(host, path)
        if scenario.unavailable:
            return 503, "Fixture server unavailable"
        if path == "/cvmfs/info/v1/repositories.json":
            repos = [{"name": name, "url": f"/cvmfs/{name}"} for name in REPOSITORIES]
            is_s0 = SERVERS[host] == "Stratum0"
            return 200, {
                "schema": 1,
                "repositories": repos if is_s0 else [],
                "replicas": [] if is_s0 else repos,
            }
        if path == "/cvmfs/info/v1/meta.json":
            return 200, {
                "administrator": "Fixture admin",
                "email": "admin@example.test",
                "organisation": "EESSI",
                "custom": {},
            }
        parts = path.split("/")
        if len(parts) >= 4 and parts[1] == "cvmfs" and parts[2] in REPOSITORIES:
            if parts[3:] == [".cvmfs_status.json"]:
                return 200, {}
            if parts[3:] == [".cvmfspublished"]:
                revision = 12 - (scenario.lag if host == "alpha.s1.test" else 0)
                return 200, (
                    "C0123456789abcdef\nB4096\nAno\nR0123456789abcdef\n"
                    "X0123456789abcdef\nGyes\nH0123456789abcdef\n"
                    f"T1781740800\nD60\nS{revision}\nN{parts[2]}\n"
                    "M0123456789abcdef\nY0123456789abcdef\n--\nfixture-signature\n"
                )
            if parts[3:6] == ["api", "v1.0", "geo"] and len(parts) == 8:
                return 200, "1,2,3"
        return self.unexpected_request(host, path)

    def unexpected_request(self, host, path):
        self.server.unexpected.append((host, path))
        return 404, "Unexpected fixture request"

    def log_message(self, *_args):
        pass


class HtmlOutputComparison(unittest.TestCase):
    maxDiff = None

    def configuration(self, scenario):
        config = json.loads((ROOT / "config.json").read_text())
        config["meta"]["title"] = "EESSI <status> & \"Å/雪\" 'test'"
        config["meta"]["contact_email"] = "ops+o'hara@example.test"
        config["servers"] = [
            {"hostname": host, "server_type": kind, "backend_type": "CVMFS"}
            for host, kind in SERVERS.items()
        ]
        config["repositories"] = REPOSITORIES
        config["ignored_repositories"] = []
        config["limit_scraping_to_repositories"] = True
        config["replication_grace_seconds"] = scenario.grace_seconds
        config["history"] = {"enabled": scenario.history}
        config["external_metrics"] = None
        if scenario.external:
            config["external_metrics"] = {
                "kind": "grafana",
                "url": "http://metrics.test",
                "datasource_uid": "fixture",
                "token_env": "HTML_COMPARISON_TOKEN",
                "stratum1_disk_usage": {
                    "query": "fixture_bytes",
                    "instance_regex": ".*",
                },
            }
        return config

    def render(self, binary, source, output, config_path, scenario, proxy):
        output.mkdir()
        status_path = "status/index.html" if scenario.nested else "index.html"
        trends_path = "capacity/trends.html" if scenario.nested else "trends.html"
        # Every HTTP request goes to our loopback fixture, never to a live server.
        env = {
            key: val for key, val in os.environ.items()
            if not key.lower().endswith("_proxy")
        }
        env.update(
            http_proxy=proxy, https_proxy=proxy, no_proxy="",
            HTML_COMPARISON_TOKEN="fixture",
        )
        result = subprocess.run(
            [
                str(binary), "--configuration", str(config_path),
                "--destination", str(output), "--output-file", status_path,
                "--trends-output-file", trends_path,
            ],
            cwd=source,
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        status = json.loads((output / "status.json").read_text())
        trends = json.loads((output / "trends.json").read_text())
        # Equal error pages must not masquerade as successful fixture rendering.
        self.assertEqual(len(status["servers_enriched"]), len(SERVERS))
        for server in status["servers_enriched"]:
            self.assertEqual(len(server["repositories"]), 0 if scenario.unavailable else 2)
        if scenario.external:
            self.assertEqual(trends["external_metrics"]["source"], "grafana")
            self.assertFalse(trends["external_metrics"]["fallback_from_history"])
        self.assertRegex(status["last_update"], r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
        self.assertIs(type(trends["generated_at"]), int)
        pages = {}
        for path, timestamp in (
            (status_path, status["last_update"]),
            (trends_path, trends["generated_at"]),
        ):
            html = (output / path).read_bytes()
            # Replace only the exact footer timestamp, verified against this run's JSON.
            # Preserve all whitespace, escaping, URLs, markup and other data verbatim.
            footer_time = f"Last updated {timestamp} |".encode()
            self.assertEqual(html.count(footer_time), 1, f"Missing footer in {path}")
            pages[path] = html.replace(footer_time, b"Last updated <TIMESTAMP> |", 1)
        status_html = pages[status_path]
        self.assertEqual(b"Recent history" in status_html, scenario.history)
        self.assertEqual(
            b"Catching up" in status_html,
            bool(scenario.lag and scenario.grace_seconds),
        )
        self.assertEqual(b"disk-usage-chart" in pages[trends_path], scenario.external)
        return pages

    def test_html_matches_reference(self):
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario.name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                config = root / "config.json"
                config.write_text(json.dumps(self.configuration(scenario)))
                with ThreadingHTTPServer(("127.0.0.1", 0), FixtureHandler) as server:
                    server.scenario = scenario
                    server.unexpected = []
                    thread = threading.Thread(target=server.serve_forever, daemon=True)
                    thread.start()
                    try:
                        proxy = f"http://127.0.0.1:{server.server_port}"
                        expected = self.render(
                            self.reference_binary, self.reference_root, root / "reference",
                            config, scenario, proxy,
                        )
                        # The pinned reference hardcodes a healthy repository overview.
                        # Accept only this reviewed correction for the two failing cases.
                        if scenario.name in {"behind_without_grace", "unavailable"}:
                            path = "status/index.html" if scenario.nested else "index.html"
                            before = (
                                b'<h2>Repositories</h2>\n'
                                b'                    <div class="content-right"><span\n'
                                b'                            class="status-ok fas fa-check '
                                b'infoblock-statusicon"></span></div>'
                            )
                            after = before.replace(
                                b"status-ok fas fa-check", b"status-failed fas fa-times-circle"
                            )
                            self.assertEqual(expected[path].count(before), 1)
                            expected[path] = expected[path].replace(before, after, 1)
                        actual = self.render(
                            self.candidate_binary, ROOT, root / "candidate",
                            config, scenario, proxy,
                        )
                        self.assertEqual(server.unexpected, [])
                        # Correct the pinned reference's unconditional GeoAPI checkmarks.
                        path = "status/index.html" if scenario.nested else "index.html"
                        geoapi_class = (
                            "muted fas fa-question-circle" if scenario.unavailable
                            else "status-ok fas fa-check"
                        )
                        description = (
                            "GeoAPI result unavailable" if scenario.unavailable
                            else "GeoAPI response received"
                        )
                        before = b'<td class="geoapi"><span class="status-ok fas fa-check"></span></td>'
                        after = (
                            f'<td class="geoapi"><span class="{geoapi_class}" '
                            f'title="{description}" aria-label="{description}"></span></td>'
                        ).encode()
                        server_html, separator, repository_html = expected[path].partition(
                            b'<div id="repositories_handler"'
                        )
                        self.assertTrue(separator)
                        self.assertEqual(server_html.count(before), 4)
                        expected[path] = (
                            server_html.replace(before, after) + separator + repository_html
                        )
                        for path in expected:
                            with self.subTest(page=path):
                                self.assertEqual(
                                    expected[path].decode(), actual[path].decode(),
                                    f"HTML differs from main: {scenario.name}/{path}",
                                )
                    finally:
                        server.shutdown()
                        thread.join()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--reference-binary", type=Path, required=True)
    parser.add_argument("--candidate-binary", type=Path, required=True)
    args = parser.parse_args()
    HtmlOutputComparison.reference_root = args.reference_root.resolve(strict=True)
    HtmlOutputComparison.reference_binary = args.reference_binary.resolve(strict=True)
    HtmlOutputComparison.candidate_binary = args.candidate_binary.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)
