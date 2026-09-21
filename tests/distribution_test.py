"""Local package fixtures: partial publication and stale taps cannot pass."""
import copy
import contextlib
import hashlib
import importlib.util
import io
from pathlib import Path
import tempfile
import shutil
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("distribution", ROOT / ".github/scripts/verify_distribution.py")
distribution = importlib.util.module_from_spec(spec)
spec.loader.exec_module(distribution)


class Distribution(unittest.TestCase):
    def setUp(self):
        deny = patch.object(distribution.subprocess, "run", side_effect=AssertionError("offline test"))
        deny.start()
        self.addCleanup(deny.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.tag = "v1.2.3"
        self.names = ["theme-aarch64-apple-darwin.tar.gz", "theme-x86_64-apple-darwin.tar.gz",
                      "theme-aarch64-unknown-linux-gnu.tar.gz", "theme-x86_64-unknown-linux-gnu.tar.gz",
                      "theme_1.2.3_amd64.deb", "theme_1.2.3_arm64.deb",
                      "theme-1.2.3-1.x86_64.rpm", "theme-1.2.3-1.aarch64.rpm"]
        self.release = {"tag_name": self.tag, "draft": False, "prerelease": False, "assets": []}
        self.digests = {}
        for name in self.names:
            self.asset(name, name.encode())
        self.asset("SHA256SUMS", "".join(f"{self.digests[n]}  {n}\n" for n in self.names).encode())
        self.formula = '  version "1.2.3"\n' + "\n".join(
            f'    on_{arch} do\n      url "https://github.com/snaraj/theme/releases/download/{self.tag}/{name}"\n'
            f'      sha256 "{self.digests[name]}"'
            for arch, name in zip(("arm", "intel", "arm", "intel"), self.names[:4]))

    def asset(self, name, data):
        (self.directory / name).write_bytes(data)
        self.digests[name] = hashlib.sha256(data).hexdigest()
        self.release["assets"] = [a for a in self.release["assets"] if a["name"] != name]
        self.release["assets"].append({"name": name, "size": len(data), "state": "uploaded",
                                       "digest": "sha256:" + self.digests[name]})

    def verify(self):
        return distribution.verify_bytes(self.release, self.tag, self.directory)

    def test_all_downloads_and_formula_agree(self):
        distribution.verify_formula(self.formula, self.tag, self.verify())

    def test_missing_duplicate_or_unpublished_assets_refuse(self):
        original = copy.deepcopy(self.release)
        for change in (lambda r: r["assets"].pop(), lambda r: r["assets"].append(r["assets"][0]),
                       lambda r: r.update(draft=True), lambda r: r.update(prerelease=True),
                       lambda r: r.update(tag_name="v1.2.2"),
                       lambda r: r["assets"][0].update(state="new"),
                       lambda r: r["assets"][0].update(size=0),
                       lambda r: r["assets"][0].update(size=64 * 1024 * 1024 + 1),
                       lambda r: r["assets"][0].update(digest="sha256:" + "0" * 64)):
            self.release = copy.deepcopy(original)
            change(self.release)
            with self.assertRaises(ValueError):
                self.verify()

    def test_invalid_tag_cannot_select_a_remote_path(self):
        for tag in ("", "../other", "v1.2.3/other", "v1.2.3-rc.1"):
            with self.assertRaises(ValueError):
                distribution.asset_names(tag)

    def test_missing_truncated_and_changed_downloads_refuse(self):
        path = self.directory / self.names[0]
        original = path.read_bytes()
        for data in (None, b"", b"x" * len(original)):
            path.unlink(missing_ok=True)
            if data is not None:
                path.write_bytes(data)
            with self.assertRaises(ValueError):
                self.verify()

    def test_checksums_are_independent_of_asset_metadata(self):
        for text in (b"", b"wrong\n", ((self.directory / "SHA256SUMS").read_bytes() * 2)):
            self.asset("SHA256SUMS", text)
            with self.assertRaises(ValueError):
                self.verify()

    def test_stale_formula_wrong_target_or_digest_refuses(self):
        for formula in (self.formula.replace('version "1.2.3"', 'version "1.2.2"'),
                        self.formula.replace('/v1.2.3/', '/v1.2.2/'),
                        self.formula.replace(self.digests[self.names[0]], "0" * 64),
                        self.formula.replace("on_arm", "on_intel", 1),
                        self.formula + self.formula):
            with self.assertRaises(ValueError):
                distribution.verify_formula(formula, self.tag, self.digests)

    def test_ci_and_postpublication_both_verify_installation(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        release = (ROOT / ".github/workflows/release.yml").read_text().split("  distribution:\n", 1)[1]
        self.assertIn(".github/scripts/prepared_release.py homebrew", ci)
        kitty = ci.split("  browser-kitty:\n", 1)[1].split("  test-macos:\n", 1)[0]
        for command in ("cargo install --path . --locked --root target/source",
                        "prepared_release.py install", "--tests-output target/release-tests",
                        "python3 -I -B target/release-tests/kitty_e2e.py",
                        "python3 -I -B target/release-tests/kitty_mutation_test.py",
                        "--theme target/source/bin/theme --output target/kitty-e2e/source",
                        '--theme "$PWD/target/installed/bin/theme" --output target/kitty-e2e/installed'):
            self.assertIn(command, kitty)
        self.assertNotIn("continue-on-error", kitty)
        for name in ("portable-smoke.sh", "browser_cli_test.py", "mdls_cli_test.py", "preview_cli_test.py"):
            self.assertIn('target/release-tests/' + name + ' "$(brew --prefix)/bin/theme"', ci)
            self.assertIn('target/release-tests/' + name + ' "$(brew --prefix)/bin/theme"', release)
        for workflow in (ci, release):
            self.assertIn("brew install snaraj/theme/theme", workflow)
            self.assertIn("brew test snaraj/theme/theme", workflow)
        self.assertIn('verify_distribution.py --tag "$TAG"', release)
        self.assertIn("needs: [slot, publish]", release)
        self.assertIn("needs.slot.outputs.authorized == 'true'", release)
        self.assertIn("needs.publish.result == 'skipped'", release)
        self.assertNotIn("continue-on-error", release)

    def test_command_rechecks_release_and_ignores_download_counters(self):
        root = self.directory / "checkout"
        (root / "Formula").mkdir(parents=True)
        (root / "Formula/theme.rb").write_text(self.formula)
        def download(command, **_):
            self.assertEqual(command[:4], ["gh", "release", "download", self.tag])
            destination = Path(command[command.index("--dir") + 1])
            for name in self.names + ["SHA256SUMS"]:
                shutil.copyfile(self.directory / name, destination / name)
        for change, passes in (({"download_count": 99}, True), ({"id": 99}, False)):
            after = copy.deepcopy(self.release)
            after["assets"][0].update(change)
            with patch.object(distribution, "ROOT", root), patch("sys.argv", ["verify_distribution.py"]), \
                 patch.object(distribution, "api", side_effect=[self.release, after]), \
                 patch.object(distribution.subprocess, "run", side_effect=download), \
                 contextlib.redirect_stdout(io.StringIO()):
                if passes:
                    distribution.main()
                else:
                    with self.assertRaises(ValueError):
                        distribution.main()

    def test_bad_formula_version_refuses_before_network(self):
        root = self.directory / "bad-formula"
        (root / "Formula").mkdir(parents=True)
        for formula in ('version "not-a-version"', self.formula + '\n' + self.formula):
            (root / "Formula/theme.rb").write_text(formula)
            with patch.object(distribution, "ROOT", root), patch("sys.argv", ["verify_distribution.py"]), \
                 patch.object(distribution, "api", side_effect=AssertionError("must not query")):
                with self.assertRaises(ValueError):
                    distribution.main()


if __name__ == "__main__":
    unittest.main()
