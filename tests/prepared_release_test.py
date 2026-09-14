"""Offline preparation receipts: source changes and substituted artifacts fail closed."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("prepared", ROOT / ".github/scripts/prepared_release.py")
prepared = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepared)
run_local = prepared.subprocess.run


class Preparation(unittest.TestCase):
    def setUp(self):
        deny = patch.object(prepared.subprocess, "run", side_effect=AssertionError("offline test"))
        deny.start()
        self.addCleanup(deny.stop)
        self.run = dict(id=7, run_attempt=1, repository={"id": prepared.REPO_ID},
                        head_repository={"id": prepared.REPO_ID}, head_sha="a" * 40,
                        path=".github/workflows/release.yml", event="workflow_dispatch",
                        head_branch="task", status="completed", conclusion="success")
        names = ["slot", *["build-" + t for t in prepared.TARGETS],
                 *["smoke-" + t for t in prepared.TARGETS if "linux" in t], "publish", "distribution"]
        self.jobs = dict(total_count=len(names), jobs=[dict(
            name=n, head_sha=self.run["head_sha"], status="completed",
            conclusion="skipped" if n in ("publish", "distribution") else "success") for n in names])
        self.artifacts = dict(total_count=4, artifacts=[dict(
            id=i + 1, name="theme-" + t, expired=False, size_in_bytes=42, digest="sha256:" + "b" * 64,
            workflow_run=dict(id=7, head_sha="a" * 40, repository_id=prepared.REPO_ID,
                              head_repository_id=prepared.REPO_ID)) for i, t in enumerate(prepared.TARGETS)])
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)

    def test_successful_dry_run_and_artifacts(self):
        prepared.validate_run(self.run, self.jobs)
        self.assertEqual(len(prepared.validate_artifacts(self.run, self.artifacts)), 4)

    def test_other_sources_workflows_events_and_failed_runs_refuse(self):
        for key, value in (("repository", {"id": 1}), ("head_repository", {"id": 1}),
                           ("path", ".github/workflows/other.yml"), ("event", "push"),
                           ("head_branch", "main"), ("head_sha", "bad"),
                           ("status", "in_progress"), ("conclusion", "failure")):
            with self.subTest(key=key):
                with self.assertRaises(ValueError):
                    prepared.validate_run(dict(self.run, **{key: value}), self.jobs)

    def test_every_required_job_must_succeed_and_publication_must_skip(self):
        for i, job in enumerate(self.jobs["jobs"]):
            for key, value in (("head_sha", "b" * 40), ("status", "queued"),
                               ("conclusion", "failure"), ("name", "unexpected")):
                with self.subTest(job=job["name"], key=key):
                    jobs = copy.deepcopy(self.jobs)
                    jobs["jobs"][i][key] = value
                    with self.assertRaises(ValueError):
                        prepared.validate_run(self.run, jobs)
        jobs = copy.deepcopy(self.jobs)
        jobs["jobs"][-1]["conclusion"] = "success"
        with self.assertRaises(ValueError):
            prepared.validate_run(self.run, jobs)
        for change in (lambda j: j.update(total_count=10), lambda j: j["jobs"].pop(),
                       lambda j: j["jobs"].append(j["jobs"][0])):
            jobs = copy.deepcopy(self.jobs)
            change(jobs)
            with self.assertRaises(ValueError):
                prepared.validate_run(self.run, jobs)

    def test_changed_expired_or_foreign_artifacts_refuse(self):
        for key, value in (("id", 0), ("id", True), ("name", "other"), ("expired", True),
                           ("size_in_bytes", 0), ("size_in_bytes", prepared.MAX_FILE * 4 + 1),
                           ("digest", "bad")):
            inventory = copy.deepcopy(self.artifacts)
            inventory["artifacts"][0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                prepared.validate_artifacts(self.run, inventory)
        for key, value in (("id", 8), ("head_sha", "b" * 40),
                           ("repository_id", 1), ("head_repository_id", 1)):
            inventory = copy.deepcopy(self.artifacts)
            inventory["artifacts"][0]["workflow_run"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                prepared.validate_artifacts(self.run, inventory)
        for change in (lambda a: a.update(total_count=5), lambda a: a["artifacts"].pop(),
                       lambda a: a["artifacts"][0].update(id=2)):
            inventory = copy.deepcopy(self.artifacts)
            change(inventory)
            with self.assertRaises(ValueError):
                prepared.validate_artifacts(self.run, inventory)

    def test_fingerprint_excludes_exactly_the_two_generated_files(self):
        source = [dict(type="blob", mode="100644", sha="a" * 40, path="src/main.rs")]
        before = prepared.fingerprint(source)
        self.assertEqual(before, prepared.fingerprint(source + [
            dict(type="blob", mode="100644", sha="b" * 40, path=p) for p in prepared.EXCLUDED]))
        for key, value in (("sha", "b" * 40), ("mode", "100755"), ("path", "src/other.rs")):
            self.assertNotEqual(before, prepared.fingerprint([dict(source[0], **{key: value})]))
        for path in ("Formula/other.rb", ".github/scripts/prepared_release.py", ".github/workflows/release.yml",
                     "README.md", "Cargo.lock"):
            self.assertNotEqual(before, prepared.fingerprint(source + [dict(source[0], path=path)]))
        with self.assertRaises(ValueError):
            prepared.fingerprint([dict(source[0], type="commit")])

    def archive(self, names=None, mode=stat.S_IFREG | 0o644):
        name = "theme-aarch64-apple-darwin.tar.gz"
        archive = self.directory / "artifact.zip"
        with zipfile.ZipFile(archive, "w") as zipped:
            for path in names or [name]:
                entry = zipfile.ZipInfo(path)
                entry.external_attr = mode << 16
                zipped.writestr(entry, b"payload")
        artifact = dict(name="theme-aarch64-apple-darwin", size_in_bytes=archive.stat().st_size,
                        digest="sha256:" + hashlib.sha256(archive.read_bytes()).hexdigest())
        return archive, artifact

    def test_exact_container_and_payload_are_verified(self):
        archive, artifact = self.archive()
        digest = prepared.unpack(archive, artifact, "v1.2.3", self.directory)
        self.assertEqual(digest, {"theme-aarch64-apple-darwin.tar.gz": hashlib.sha256(b"payload").hexdigest()})
        for key, value in (("size_in_bytes", 1), ("digest", "sha256:" + "0" * 64)):
            with self.assertRaises(ValueError):
                prepared.unpack(archive, dict(artifact, **{key: value}), "v1.2.3", self.directory)

    def test_payload_inventory_paths_and_types_refuse(self):
        for paths, mode in ((["../unexpected"], stat.S_IFREG), (["unexpected"], stat.S_IFREG),
                            (["theme-aarch64-apple-darwin.tar.gz", "extra"], stat.S_IFREG),
                            (None, stat.S_IFLNK)):
            archive, artifact = self.archive(paths, mode)
            with self.assertRaises(ValueError):
                prepared.unpack(archive, artifact, "v1.2.3", self.directory)
        archive, artifact = self.archive()
        with patch.object(prepared, "MAX_FILE", 1), self.assertRaises(ValueError):
            prepared.unpack(archive, artifact, "v1.2.3", self.directory)

    def test_receipt_attempt_source_and_payload_substitution_refuse(self):
        source = [dict(type="blob", mode="100644", sha="a" * 40, path="src/main.rs")]
        files = {n: "c" * 64 for n in prepared.DISTRIBUTION["asset_names"]("v1.2.3")}
        receipt = dict(schema=1, tag="v1.2.3", run_id=7, run_attempt=1, source_sha="a" * 40,
                       source_fingerprint=prepared.fingerprint(source),
                       artifacts=prepared.validate_artifacts(self.run, self.artifacts), files=files)
        def api(path):
            if path == "actions/runs/7":
                return self.run
            if path == "actions/runs/7/attempts/1/jobs?per_page=100":
                return self.jobs
            if path == "git/trees/" + "a" * 40 + "?recursive=1":
                return dict(truncated=False, tree=source)
            if path == "actions/runs/7/artifacts?per_page=100":
                return self.artifacts
            raise AssertionError(path)
        with patch.object(prepared, "api", side_effect=api), \
             patch.object(prepared, "local_fingerprint", return_value=prepared.fingerprint(source)), \
             patch.object(prepared.subprocess, "run"), patch.object(prepared, "unpack", return_value=files):
            self.assertEqual(prepared.collect(7, "v1.2.3", self.directory, receipt), receipt)
            for key, value in (("schema", 2), ("source_sha", "b" * 40), ("run_attempt", 2),
                               ("source_fingerprint", "b" * 64), ("files", {"other": "c" * 64})):
                with self.subTest(key=key), self.assertRaises(ValueError):
                    prepared.collect(7, "v1.2.3", self.directory, dict(receipt, **{key: value}))
            with patch.object(prepared, "local_fingerprint", return_value="changed"), self.assertRaises(ValueError):
                prepared.collect(7, "v1.2.3", self.directory, receipt)
            latest = copy.deepcopy(self.run)
            def raced(path):
                if path == "actions/runs/7":
                    value = copy.deepcopy(latest)
                    latest["run_attempt"] += 1
                    return value
                return api(path)
            with patch.object(prepared, "api", side_effect=raced), self.assertRaises(ValueError):
                prepared.collect(7, "v1.2.3", self.directory, receipt)

    def test_workflow_isolates_preparation_and_requires_verification_before_publish(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        build = workflow.split("  build:\n", 1)[1].split("  smoke:\n", 1)[0]
        publish = workflow.split("  publish:\n", 1)[1].split("  distribution:\n", 1)[0]
        self.assertIn("if: github.event_name == 'workflow_dispatch' && github.ref != 'refs/heads/main'", build)
        self.assertIn("git archive HEAD | tar --exclude=Formula/theme.rb --exclude=.github/release-preparation.json", build)
        self.assertIn("        shell: bash\n", build)
        self.assertIn("working-directory: ${{ github.workspace }}/../theme-source", build)
        self.assertIn('git -C "$GITHUB_WORKSPACE/../theme-source" rev-parse --show-toplevel', build)
        self.assertIn("retention-days: 90", build)
        self.assertIn("if: needs.slot.outputs.publish == 'true'", publish)
        self.assertIn("ref: ${{ needs.slot.outputs.source_sha }}", publish)
        self.assertLess(publish.index("prepared_release.py verify --output prepared"), publish.index("- id: tag"))
        self.assertIn("working-directory: prepared", publish)
        self.assertNotIn("cargo build", publish)
        self.assertIn("actions: read", publish)

    def test_real_export_has_no_git_history_and_a_failed_producer_fails(self):
        destination = self.directory / "export"
        destination.mkdir()
        script = 'git archive HEAD | tar --exclude=Formula/theme.rb --exclude=.github/release-preparation.json -xf - -C "$1"'
        run_local(["/bin/bash", "-eo", "pipefail", "-c", script, "export", str(destination)],
                  cwd=ROOT, check=True, capture_output=True)
        self.assertFalse((destination / "Formula/theme.rb").exists())
        self.assertFalse((destination / ".github/release-preparation.json").exists())
        self.assertTrue((destination / "Cargo.toml").is_file())
        discovery = run_local(["git", "-C", str(destination), "rev-parse", "--show-toplevel"], capture_output=True)
        self.assertNotEqual(discovery.returncode, 0)
        failed = script.replace("git archive HEAD", "(git archive HEAD; exit 7)")
        result = run_local(["/bin/bash", "-eo", "pipefail", "-c", failed, "export", str(destination)],
                           cwd=ROOT, capture_output=True)
        self.assertEqual(result.returncode, 7)

    def test_homebrew_uses_verified_cache_only_for_an_unpublished_release(self):
        root = self.directory / "source"
        (root / "Formula").mkdir(parents=True)
        (root / "Formula/theme.rb").write_text('  version "1.2.3"\n')
        filename = "theme-aarch64-apple-darwin.tar.gz"
        cache = self.directory / "cache" / ("url-hash--" + filename)
        calls = []
        def run(command, **kwargs):
            calls.append(command)
            if command[:2] == ["gh", "api"]:
                return SimpleNamespace(returncode=1, stderr="gh: Not Found (HTTP 404)")
            self.assertEqual(command, ["brew", "--cache", "--formula", "snaraj/theme/theme"])
            return SimpleNamespace(stdout=str(cache) + "\n")
        def verify(directory):
            (directory / filename).write_bytes(b"verified payload")
        with patch.object(prepared, "ROOT", root), patch.object(prepared.subprocess, "run", side_effect=run), \
             patch.object(prepared, "verify", side_effect=verify) as verification:
            prepared.homebrew()
            self.assertEqual(cache.read_bytes(), b"verified payload")
            verification.assert_called_once()
            self.assertEqual(len(calls), 2)
        for error in ("gh: Forbidden (HTTP 403)", "network unavailable"):
            with patch.object(prepared, "ROOT", root), \
                 patch.object(prepared.subprocess, "run", return_value=SimpleNamespace(returncode=1, stderr=error)), \
                 patch.object(prepared, "verify", side_effect=AssertionError("must not fall back")), \
                 self.assertRaises(ValueError):
                prepared.homebrew()
        with patch.object(prepared, "ROOT", root), \
             patch.object(prepared.subprocess, "run", return_value=SimpleNamespace(returncode=0)) as command, \
             patch.object(prepared, "verify", side_effect=AssertionError("published release must use published verifier")):
            prepared.homebrew()
            self.assertEqual(command.call_args_list[-1].args[0],
                             ["python3", "-I", "-B", str(root / ".github/scripts/verify_distribution.py")])


if __name__ == "__main__":
    unittest.main()
