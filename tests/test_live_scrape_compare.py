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
        if self.server.redirect_status and self.path.endswith(".cvmfspublished"):
            self.send_response(self.server.redirect_status)
            self.send_header("Location", "/manifest")
            self.end_headers()
            return
        body = str(self.server.requests).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


@contextmanager
def upstream(redirect_status=None):
    with ThreadingHTTPServer(("127.0.0.1", 0), UpstreamHandler) as server:
        server.requests = 0
        server.redirect_status = redirect_status
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
if behavior == "no_redirects":
    class NoRedirects(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *_args, **_kwargs):
            return None
    urllib.request.install_opener(urllib.request.build_opener(NoRedirects()))
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

    def test_redirect_chain_is_captured_and_replayed_with_upstream_offline(self):
        for status in [301, 302, 303, 307, 308]:
            with self.subTest(status=status):
                with upstream(redirect_status=status) as server:
                    args = self.arguments(server, directory=f"redirect-{status}")
                    self.assertEqual(compare(args), 0)
                    self.assertEqual(server.requests, 2)
                cassette = json.loads((args.report_dir / "cassette.json").read_text())
                original_url = f"http://127.0.0.1:{server.server_port}/cvmfs/repo/.cvmfspublished"
                self.assertEqual(cassette["responses"][original_url]["status"], status)
                self.assertEqual(cassette["responses"][original_url]["location"], "/manifest")
                self.assertEqual(len(cassette["responses"]), 2)
                args.cassette_in = args.report_dir / "cassette.json"
                args.report_dir = self.root / f"redirect-replay-{status}"
                self.assertEqual(compare(args), 0)

    def test_approval_cannot_hide_a_candidate_that_stops_following_redirects(self):
        with upstream(redirect_status=302) as server:
            args = self.arguments(server, "no_redirects")
            args.allow_divergence = True
            with self.assertRaisesRegex(ValueError, "candidate exited"):
                compare(args)
            self.assertIn("HTTP Error 302", (args.report_dir / "candidate.log").read_text())

    def test_replay_rejects_captures_that_flattened_redirects(self):
        with upstream() as server:
            args = self.arguments(server)
            self.assertEqual(compare(args), 0)
        args.cassette_in = args.report_dir / "cassette.json"
        cassette = json.loads(args.cassette_in.read_text())
        cassette["v"] = 1
        args.cassette_in.write_text(json.dumps(cassette))
        args.report_dir = self.root / "old-capture"
        with self.assertRaisesRegex(ValueError, "Cassette version"):
            compare(args)

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
        files = {"context.json": json.dumps(dict(context, base="c" * 40))}
        pages = [{"artifacts": [{"id": 1, "expired": False, "workflow_run": {"id": 9}}]}]
        run = {
            "path": ".github/workflows/live-scrape.yml", "event": "pull_request",
            "head_sha": context["head"], "status": "completed",
        }
        with (
            patch.dict(os.environ, GITHUB_REPOSITORY="owner/repo"),
            patch("live_scrape_ci.gh_json", side_effect=[pages, run]),
            patch("live_scrape_ci.subprocess.run", side_effect=self.download_capture(files)),
        ):
            with self.assertRaisesRegex(ValueError, "does not belong"):
                restore_capture(context, self.root)

    @staticmethod
    def download_capture(files):
        def download(command, **_kwargs):
            destination = Path(command[command.index("--dir") + 1])
            for name, contents in files.items():
                path = destination / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)
        return download

    @staticmethod
    def completed_capture(context):
        return {
            "context.json": json.dumps(context),
            "summary.json": json.dumps({"different": True}),
            "cassette.json": json.dumps({"v": 2, "hosts": ["fixture"], "responses": {}}),
            "config.json": json.dumps({"servers": [{"hostname": "fixture"}]}),
            "diff.txt": "reviewed difference\n",
            **{
                f"{kind}/{name}": "reviewed output\n"
                for kind in ["reference", "candidate"]
                for name in ["status.json", "trends.json", "metrics"]
            },
        }

    @contextmanager
    def artifact_history(self, context, captures):
        pages = [{"artifacts": [
            {"id": index, "expired": False, "workflow_run": {"id": index}}
            for index in range(len(captures))
        ]}]
        run = {
            "path": ".github/workflows/live-scrape.yml", "event": "pull_request",
            "head_sha": context["head"], "status": "completed",
        }

        def download(command, **kwargs):
            self.download_capture(captures[int(command[3])])(command, **kwargs)

        with (
            patch.dict(os.environ, GITHUB_REPOSITORY="owner/repo"),
            patch("live_scrape_ci.gh_json", side_effect=[pages] + [run] * len(captures)),
            patch("live_scrape_ci.subprocess.run", side_effect=download) as downloads,
        ):
            yield downloads

    def test_approval_skips_incomplete_artifacts_without_mixing_their_files(self):
        context = {"head": "a" * 40, "base": "b" * 40}
        reviewed = self.completed_capture(context)
        incomplete = {
            "context_only": {"context.json": reviewed["context.json"]},
            "missing_cassette": {k: v for k, v in reviewed.items() if k != "cassette.json"},
            "missing_config": {k: v for k, v in reviewed.items() if k != "config.json"},
            "missing_diff": {k: v for k, v in reviewed.items() if k != "diff.txt"},
            "missing_output": {k: v for k, v in reviewed.items() if k != "candidate/metrics"},
            "failed_comparison": dict(reviewed, **{"error.txt": "comparison failed"}),
        }
        for name, files in incomplete.items():
            with self.subTest(name=name), self.artifact_history(context, [reviewed, files]) as downloads:
                destination = self.root / name
                restore_capture(context, destination)
                self.assertEqual([call.args[0][3] for call in downloads.call_args_list], ["1", "0"])
                restored = {
                    str(path.relative_to(destination)): path.read_text()
                    for path in destination.rglob("*") if path.is_file()
                }
                self.assertEqual(restored, reviewed)

    def test_approval_fails_when_only_incomplete_captures_exist(self):
        context = {"head": "a" * 40, "base": "b" * 40}
        files = {"context.json": json.dumps(context)}
        with self.artifact_history(context, [files]):
            destination = self.root / "restored"
            with self.assertRaisesRegex(ValueError, "No reviewed capture"):
                restore_capture(context, destination)
            self.assertFalse(destination.exists())

    def test_approval_does_not_skip_a_completed_comparison_with_equal_output(self):
        context = {"head": "a" * 40, "base": "b" * 40}
        reviewed = self.completed_capture(context)
        matching = dict(reviewed, **{"summary.json": json.dumps({"different": False})})
        with self.artifact_history(context, [reviewed, matching]) as downloads:
            with self.assertRaisesRegex(ValueError, "no completed divergence"):
                restore_capture(context, self.root / "restored")
            self.assertEqual(len(downloads.call_args_list), 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
