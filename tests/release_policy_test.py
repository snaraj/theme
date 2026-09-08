"""Offline checks for publication authority; no network or subprocess execution."""
import copy
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("policy", Path(__file__).resolve().parents[1] / ".github/scripts/authorize_release.py")
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class Publication(unittest.TestCase):
    def setUp(self):
        deny = patch.object(policy.subprocess, "run", side_effect=AssertionError("offline tests"))
        deny.start()
        self.addCleanup(deny.stop)
        self.source = "a" * 40
        self.run = {"repository": {"id": policy.REPOSITORY_ID}, "head_repository": {"id": policy.REPOSITORY_ID},
                    "event": "push", "head_branch": "main", "head_sha": self.source,
                    "path": ".github/workflows/ci.yml", "status": "completed", "conclusion": "success"}
        self.jobs = {"total_count": 2, "jobs": [
            {"name": name, "head_sha": self.source, "status": "completed", "conclusion": "success"}
            for name in ("lint-test", "test-macos")]}
        self.branch = {"name": "main", "protected": True}
        self.comparison = {"status": "ahead", "merge_base_commit": {"sha": self.source}}

    def check(self):
        return policy.validate(self.source, self.run, self.jobs, self.branch, self.comparison)

    def test_successful_main_source(self):
        self.check()
        self.comparison["status"] = "identical"
        self.check()

    def test_matching_but_invalid_source_is_refused(self):
        self.source = "not-a-commit"
        self.run["head_sha"] = self.source
        self.comparison["merge_base_commit"]["sha"] = self.source
        for job in self.jobs["jobs"]:
            job["head_sha"] = self.source
        with self.assertRaises(ValueError):
            self.check()

    def test_truncated_or_extra_job_inventory_is_refused(self):
        self.jobs["total_count"] = 3
        with self.assertRaises(ValueError):
            self.check()
        self.jobs["jobs"].append(copy.deepcopy(self.jobs["jobs"][0]))
        with self.assertRaises(ValueError):
            self.check()

    def test_foreign_or_wrong_ci_is_refused(self):
        for field, value in (("event", "pull_request"), ("head_branch", "other"), ("head_sha", "b" * 40),
                             ("path", ".github/workflows/other.yml"), ("status", "in_progress"),
                             ("conclusion", "neutral"), ("repository", {"id": 1}), ("head_repository", {"id": 1})):
            with self.subTest(field=field):
                old = self.run[field]
                self.run[field] = value
                with self.assertRaises(ValueError):
                    self.check()
                self.run[field] = old

    def test_missing_skipped_duplicate_or_wrong_head_jobs_are_refused(self):
        original = copy.deepcopy(self.jobs)
        for field, value in (("conclusion", "skipped"), ("conclusion", "failure"), ("status", "queued"),
                             ("head_sha", "b" * 40), ("name", "lint-test")):
            with self.subTest(field=field, value=value):
                self.jobs = copy.deepcopy(original)
                self.jobs["jobs"][1][field] = value
                with self.assertRaises(ValueError):
                    self.check()
        self.jobs = original
        self.jobs["jobs"].pop()
        with self.assertRaises(ValueError):
            self.check()

    def test_protected_ancestry_is_required(self):
        self.branch["protected"] = False
        with self.assertRaises(ValueError):
            self.check()
        self.branch["protected"] = True
        for value in ("behind", "diverged"):
            self.comparison["status"] = value
            with self.assertRaises(ValueError):
                self.check()
        self.comparison["status"] = "ahead"
        self.comparison["merge_base_commit"]["sha"] = "b" * 40
        with self.assertRaises(ValueError):
            self.check()

    def invoke(self, event="workflow_dispatch", ref="refs/heads/main", rerun=False):
        reads = []
        self.run.update(id=7, run_attempt=1)
        def api(path):
            reads.append(path)
            if path.startswith("actions/workflows/ci.yml/runs?"):
                return {"total_count": 1, "workflow_runs": [self.run]}
            if path == "actions/runs/7":
                value = copy.deepcopy(self.run)
                if rerun and reads.count(path) > 1:
                    value["run_attempt"] = 2
                return value
            if path == "actions/runs/7/attempts/1/jobs?per_page=100":
                return self.jobs
            if path == "branches/main":
                return self.branch
            if path == "compare/" + self.source + "...main":
                return self.comparison
            raise AssertionError("unexpected API read")
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "event").write_text(json.dumps({"workflow_run": {"id": 7, "head_sha": self.source}}))
            env = {"GITHUB_EVENT_NAME": event, "GITHUB_REF": ref, "GITHUB_SHA": self.source,
                   "GITHUB_REPOSITORY": "snaraj/theme", "GITHUB_REPOSITORY_ID": str(policy.REPOSITORY_ID),
                   "GITHUB_EVENT_PATH": str(root / "event"), "GITHUB_OUTPUT": str(root / "output")}
            with patch.dict(os.environ, env, clear=True), patch.object(policy, "api", api), contextlib.redirect_stdout(io.StringIO()):
                try:
                    policy.main()
                except ValueError:
                    self.assertFalse((root / "output").exists(), "refusal wrote authorization outputs")
                    raise
            return (root / "output").read_text(), reads

    def test_branch_dispatch_is_a_dry_run_without_api_authority(self):
        output, reads = self.invoke(ref="refs/heads/task")
        self.assertIn("authorized=false\n", output)
        self.assertEqual(reads, [])

    def test_main_dispatch_and_completion_use_exact_attempt(self):
        for event in ("workflow_dispatch", "workflow_run"):
            output, reads = self.invoke(event=event)
            self.assertEqual(output, "source_sha=" + self.source + "\nauthorized=true\n")
            self.assertIn("actions/runs/7/attempts/1/jobs?per_page=100", reads)
            self.assertEqual(reads.count("actions/runs/7"), 2)

    def test_rerun_or_bad_ci_emits_no_authorization(self):
        with self.assertRaises(ValueError):
            self.invoke(rerun=True)
        self.jobs["jobs"][0]["conclusion"] = "skipped"
        with self.assertRaises(ValueError):
            self.invoke()

    def test_tag_push_cannot_authorize(self):
        with self.assertRaises(ValueError):
            self.invoke(event="push", ref="refs/tags/v1.2.3")


if __name__ == "__main__":
    unittest.main()
