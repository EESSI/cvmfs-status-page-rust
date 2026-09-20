"""Deterministic coverage of live capture, replay, normalization, and approval."""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from live_scrape_ci import restore_capture, review_context
from live_scrape_compare import compare, normalize_json, normalize_metrics, request_key


class UpstreamHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.requests += 1
        body = str(self.server.requests).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


@contextmanager
def upstream():
    with ThreadingHTTPServer(("127.0.0.1", 0), UpstreamHandler) as server:
        server.requests = 0
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server
        finally:
            server.shutdown()
            thread.join()


GENERATOR = r'''
import json, pathlib, sys, time, urllib.request
behavior = BEHAVIOR
if behavior == "crash":
    sys.exit(4)
args = sys.argv
config = json.loads(pathlib.Path(args[args.index("--configuration") + 1]).read_text())
output = pathlib.Path(args[args.index("--destination") + 1])
output.mkdir(parents=True)
url = "http://" + config["servers"][0]["hostname"] + "/cvmfs/repo/.cvmfspublished"
revision = int(urllib.request.urlopen(url, timeout=5).read())
if behavior == "extra_request":
    urllib.request.urlopen(url + "-unrecorded", timeout=5).read()
state = "FAILED" if behavior == "changed" else "OK"
status = {group: {"status": state} for group in ["eessi_status", "stratum0", "stratum1", "syncservers", "repositories_status"]}
status.update(
    last_update="2026-09-20T12:00:00Z",
    servers=[{"name": "fixture", "status": state}],
    servers_enriched=[{"repositories": [{"name": "repo", "revision": revision}]}],
)
(output / "status.json").write_text("invalid JSON" if behavior == "invalid" else json.dumps(status))
(output / "trends.json").write_text(json.dumps({"v": 1, "generated_at": int(time.time())}))
(output / "metrics").write_text("eessi_status 0 1000\nrepositories_status 0 1000\n")
'''


class LiveComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def binary(self, name, behavior):
        path = self.root / name
        path.write_text("#!" + sys.executable + "\n" + GENERATOR.replace("BEHAVIOR", repr(behavior)))
        path.chmod(0o755)
        return path

    def arguments(self, server, behavior="same", directory="report"):
        config = self.root / "config.json"
        config.write_text(json.dumps({"servers": [{"hostname": f"127.0.0.1:{server.server_port}"}]}))
        return SimpleNamespace(
            reference_binary=self.binary("reference", "same"), reference_root=self.root,
            candidate_binary=self.binary("candidate", behavior), candidate_root=self.root,
            configuration=config, report_dir=self.root / directory,
            cassette_in=None, allow_divergence=False,
        )

    def test_both_binaries_receive_one_capture_even_when_upstream_changes(self):
        with upstream() as server:
            args = self.arguments(server)
            self.assertEqual(compare(args), 0)
            self.assertEqual(server.requests, 1)
            summary = json.loads((args.report_dir / "summary.json").read_text())
            self.assertFalse(summary["different"])

    def test_approval_reuses_reviewed_capture_with_upstream_offline(self):
        with upstream() as server:
            args = self.arguments(server, "changed")
            self.assertEqual(compare(args), 2)
            self.assertEqual(server.requests, 1)
        reviewed_diff = (args.report_dir / "diff.txt").read_text()
        args.cassette_in = args.report_dir / "cassette.json"
        args.report_dir = self.root / "approved"
        args.allow_divergence = True
        self.assertEqual(compare(args), 0)
        self.assertEqual((args.report_dir / "diff.txt").read_text(), reviewed_diff)
        self.assertTrue(json.loads((args.report_dir / "summary.json").read_text())["approved"])

    def test_approval_cannot_hide_crashes_invalid_outputs_or_replay_misses(self):
        for behavior in ["crash", "invalid", "extra_request"]:
            with self.subTest(behavior=behavior), upstream() as server:
                args = self.arguments(server, behavior, behavior)
                args.allow_divergence = True
                with self.assertRaises(ValueError):
                    compare(args)
                self.assertEqual(server.requests, 1)

    def test_replay_rejects_configuration_changes(self):
        with upstream() as server:
            args = self.arguments(server)
            self.assertEqual(compare(args), 0)
        args.cassette_in = args.report_dir / "cassette.json"
        args.report_dir = self.root / "replay"
        config = json.loads(args.configuration.read_text())
        config["replication_grace_seconds"] = 123
        args.configuration.write_text(json.dumps(config))
        with self.assertRaisesRegex(ValueError, "different configuration"):
            compare(args)

    def test_normalization_keeps_revisions_and_upstream_metadata(self):
        first = {
            "last_update": "2026-09-20T12:00:00Z",
            "servers": [{"name": "z"}, {"name": "a"}],
            "metadata": {"last_update": "upstream", "generated_at": 42},
            "repositories_enriched": [{"name": "repo", "stratum0_revision": 12}],
        }
        second = dict(first, last_update="2026-09-20T12:10:00Z", servers=list(reversed(first["servers"])))
        self.assertEqual(normalize_json(first), normalize_json(second))
        self.assertEqual(normalize_json(first)["metadata"], first["metadata"])
        second["repositories_enriched"] = [{"name": "repo", "stratum0_revision": 13}]
        self.assertNotEqual(normalize_json(first), normalize_json(second))

    def test_metrics_ignore_only_order_and_sample_timestamps(self):
        first = "eessi_status 0 1000\nrepositories_status 3 1000\n"
        second = "repositories_status 3 2000\neessi_status 0 2000\n"
        self.assertEqual(normalize_metrics(first), normalize_metrics(second))
        self.assertNotEqual(normalize_metrics(first), normalize_metrics(second.replace("status 3", "status 0")))
        with self.assertRaises(ValueError):
            normalize_metrics("not metrics\n")

    def test_geoapi_nonce_is_the_only_url_component_ignored(self):
        base = "http://fixture/cvmfs/repo/api/v1.0/geo/nonce/host1,host2"
        self.assertEqual(request_key(base), request_key(base.replace("nonce", "new-nonce")))
        self.assertNotEqual(request_key(base), request_key(base.replace("repo/", "other/")))
        self.assertNotEqual(request_key(base), request_key(base.replace("host1,host2", "host2,host1")))

    def test_only_applying_approval_label_approves_current_head(self):
        event = {
            "action": "labeled", "label": {"name": "approve-output-divergence"},
            "pull_request": {
                "head": {"sha": "a" * 40}, "base": {"sha": "b" * 40},
                "labels": [{"name": "live-scrape"}, {"name": "approve-output-divergence"}],
            },
        }
        context, approved = review_context(event)
        self.assertTrue(approved)
        self.assertEqual(context["head"], "a" * 40)
        for action in ["synchronize", "reopened", "unlabeled", "opened"]:
            with self.subTest(action=action):
                self.assertFalse(review_context(dict(event, action=action))[1])
        event["label"]["name"] = "unrelated"
        self.assertFalse(review_context(event)[1])

    def test_approval_cannot_restore_another_commits_artifact(self):
        context = {"head": "a" * 40, "base": "b" * 40}
        pages = [{"artifacts": [{"id": 1, "expired": False, "workflow_run": {"id": 9}}]}]
        run = {
            "path": ".github/workflows/live-scrape.yml", "event": "pull_request",
            "head_sha": "c" * 40, "status": "completed",
        }
        with (
            patch.dict(os.environ, GITHUB_REPOSITORY="owner/repo"),
            patch("live_scrape_ci.gh_json", side_effect=[pages, run]),
            patch("live_scrape_ci.subprocess.run") as download,
        ):
            with self.assertRaisesRegex(ValueError, "No reviewed capture"):
                restore_capture(context, self.root)
            download.assert_not_called()

    def test_approval_rejects_an_artifact_with_wrong_base_metadata(self):
        context = {"head": "a" * 40, "base": "b" * 40}
        (self.root / "context.json").write_text(json.dumps(dict(context, base="c" * 40)))
        pages = [{"artifacts": [{"id": 1, "expired": False, "workflow_run": {"id": 9}}]}]
        run = {
            "path": ".github/workflows/live-scrape.yml", "event": "pull_request",
            "head_sha": context["head"], "status": "completed",
        }
        with (
            patch.dict(os.environ, GITHUB_REPOSITORY="owner/repo"),
            patch("live_scrape_ci.gh_json", side_effect=[pages, run]),
            patch("live_scrape_ci.subprocess.run"),
        ):
            with self.assertRaisesRegex(ValueError, "does not belong"):
                restore_capture(context, self.root)


if __name__ == "__main__":
    unittest.main(verbosity=2)
