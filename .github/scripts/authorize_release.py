"""Bind publication to protected main and its exact, successful CI attempt."""
import json
import os
import re
import subprocess
from pathlib import Path

REPOSITORY = "snaraj/theme"
REPOSITORY_ID = 1353261670
CHECKS = {"lint-test", "test-macos", "browser-kitty"}


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def validate(source, run, jobs, branch, comparison):
    require(re.fullmatch(r"[0-9a-f]{40}", source) is not None, "invalid source")
    require(run.get("repository", {}).get("id") == REPOSITORY_ID
            and run.get("head_repository", {}).get("id") == REPOSITORY_ID,
            "foreign CI repository")
    require((run.get("event"), run.get("head_branch"), run.get("head_sha"), run.get("path"))
            == ("push", "main", source, ".github/workflows/ci.yml"), "wrong CI source or workflow")
    require((run.get("status"), run.get("conclusion")) == ("completed", "success"), "CI did not succeed")
    require(branch.get("name") == "main" and branch.get("protected") is True, "main is not protected")
    require(comparison.get("status") in ("ahead", "identical")
            and comparison.get("merge_base_commit", {}).get("sha") == source,
            "source is not in protected main")
    records = jobs.get("jobs", [])
    require(jobs.get("total_count") == len(records) == len(CHECKS), "incomplete CI job inventory")
    require({j.get("name") for j in records} == CHECKS, "required CI job missing")
    require(all(j.get("head_sha") == source and j.get("status") == "completed"
                and j.get("conclusion") == "success" for j in records), "required CI job did not succeed")


def api(suffix):
    done = subprocess.run(["gh", "api", "repos/" + REPOSITORY + "/" + suffix],
                          check=True, capture_output=True, text=True, timeout=30)
    return json.loads(done.stdout)


def main():
    require(os.environ.get("GITHUB_REPOSITORY") == REPOSITORY
            and os.environ.get("GITHUB_REPOSITORY_ID") == str(REPOSITORY_ID), "foreign repository")
    event_name = os.environ["GITHUB_EVENT_NAME"]
    require(event_name in ("workflow_run", "workflow_dispatch"), "unsupported release event")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    source = event.get("workflow_run", {}).get("head_sha") if event_name == "workflow_run" else os.environ["GITHUB_SHA"]
    require(isinstance(source, str) and re.fullmatch(r"[0-9a-f]{40}", source), "invalid source")
    publish = event_name == "workflow_run" or os.environ["GITHUB_REF"] == "refs/heads/main"
    if publish:
        if event_name == "workflow_run":
            run_id = event["workflow_run"]["id"]
        else:
            runs = api("actions/workflows/ci.yml/runs?event=push&branch=main&head_sha=" + source + "&per_page=100")
            candidates = runs.get("workflow_runs", [])
            require(candidates and runs.get("total_count") == len(candidates), "CI inventory missing or truncated")
            run_id = max(candidates, key=lambda r: r["id"])["id"]
        require(type(run_id) is int and run_id > 0, "invalid CI run")
        run = api("actions/runs/" + str(run_id))
        attempt = run.get("run_attempt")
        require(type(attempt) is int and attempt > 0, "invalid CI attempt")
        jobs = api(f"actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100")
        validate(source, run, jobs, api("branches/main"), api("compare/" + source + "...main"))
        # A re-run invalidates the evidence collected for the previous attempt.
        latest = api("actions/runs/" + str(run_id))
        require(latest.get("run_attempt") == attempt and latest.get("status") == "completed"
                and latest.get("conclusion") == "success", "CI changed during authorization")
    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
        output.write(f"source_sha={source}\nauthorized={str(publish).lower()}\n")
    print("RELEASE_SOURCE=" + source + " AUTHORIZED=" + str(publish).lower())


if __name__ == "__main__":
    main()
