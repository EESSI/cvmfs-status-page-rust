"""Bind divergence approval and the reviewed capture to an exact PR head/base."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


APPROVAL_LABEL = "approve-output-divergence"
CAPTURE_FILES = (
    "summary.json", "cassette.json", "config.json", "diff.txt",
    "reference/status.json", "reference/trends.json", "reference/metrics",
    "candidate/status.json", "candidate/trends.json", "candidate/metrics",
)


def review_context(event):
    pr = event["pull_request"]
    context = {name: pr[name]["sha"] for name in ("head", "base")}
    if any(not re.fullmatch(r"[0-9a-f]{40}", sha) for sha in context.values()):
        raise ValueError("Invalid commit SHA in pull request event")
    approved = (
        event["action"] == "labeled"
        and event.get("label", {}).get("name") == APPROVAL_LABEL
        and "live-scrape" in {label["name"] for label in pr["labels"]}
    )
    return context, approved


def gh_json(*args):
    return json.loads(subprocess.check_output(["gh", *args], text=True))


def restore_capture(context, destination):
    repository = os.environ["GITHUB_REPOSITORY"]
    artifact_name = f"live-scrape-{context['head']}-{context['base']}"
    pages = gh_json("api", "--paginate", "--slurp", f"repos/{repository}/actions/artifacts?name={artifact_name}&per_page=100")
    artifacts = sorted(
        (artifact for page in pages for artifact in page["artifacts"] if not artifact["expired"]),
        key=lambda artifact: artifact["id"], reverse=True,
    )
    for artifact in artifacts:
        run_id = artifact["workflow_run"]["id"]
        run = gh_json("api", f"repos/{repository}/actions/runs/{run_id}")
        if (
            run["path"] != ".github/workflows/live-scrape.yml"
            or run["event"] != "pull_request"
            or run["head_sha"] != context["head"]
            or run["status"] != "completed"
        ):
            continue
        # Failed or cancelled runs can upload only part of a report. Inspect each
        # artifact in isolation so skipped files cannot contaminate the capture.
        with tempfile.TemporaryDirectory() as temporary:
            capture = Path(temporary)
            subprocess.run(
                ["gh", "run", "download", str(run_id), "--repo", repository,
                 "--name", artifact_name, "--dir", str(capture)], check=True,
            )
            context_path = capture / "context.json"
            if not context_path.is_file():
                continue
            if json.loads(context_path.read_text()) != context:
                raise ValueError("Capture does not belong to this PR head/base")
            if (capture / "error.txt").exists() or any(
                not (capture / name).is_file() for name in CAPTURE_FILES
            ):
                continue
            summary = json.loads((capture / "summary.json").read_text())
            if not summary["different"]:
                raise ValueError("The previous run has no completed divergence to approve")
            shutil.copytree(capture, destination, dirs_exist_ok=True)
            return
    raise ValueError("No reviewed capture for this head/base; run live-scrape and inspect its diff first")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event-file", type=Path, required=True)
    parser.add_argument("--report-dir", type=Path, required=True)
    parser.add_argument("--replay-dir", type=Path, required=True)
    args = parser.parse_args()
    context, approved = review_context(json.loads(args.event_file.read_text()))
    args.report_dir.mkdir(parents=True, exist_ok=True)
    (args.report_dir / "context.json").write_text(json.dumps(context, indent=2) + "\n")
    if approved:
        restore_capture(context, args.replay_dir)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"approved={str(approved).lower()}\n")


if __name__ == "__main__":
    main()
