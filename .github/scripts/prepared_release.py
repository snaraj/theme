"""Bind reviewed Homebrew pins to tested branch artifacts, then promote those bytes."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import runpy
import shutil
import stat
import subprocess
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
DISTRIBUTION = runpy.run_path(str(ROOT / ".github/scripts/verify_distribution.py"))
require = DISTRIBUTION["require"]
api = DISTRIBUTION["api"]
TARGETS = DISTRIBUTION["TARGETS"]
RECEIPT = ROOT / ".github/release-preparation.json"
EXCLUDED = {"Formula/theme.rb", ".github/release-preparation.json"}
REPO_ID = 1353261670
MAX_FILE = 64 * 1024 * 1024
INSTALLED_TESTS = ("portable-smoke.sh", "browser_cli_test.py", "mdls_cli_test.py",
                   "preview_cli_test.py", "cli_maintenance_test.py", "kitty_e2e.py",
                   "kitty_pixels.py", "kitty_controller.py", "kitty_mutation_test.py")


def fingerprint(entries):
    """Commit-independent identity of every source input, including file modes."""
    rows = []
    for item in entries:
        if item["type"] == "tree" or item["path"] in EXCLUDED:
            continue
        require(item["type"] == "blob", "unsupported source entry")
        rows.append(f'{item["mode"]} {item["sha"]}\t{item["path"]}\0')
    return hashlib.sha256("".join(sorted(rows)).encode()).hexdigest()


def local_fingerprint():
    raw = subprocess.run(["git", "ls-tree", "-rz", "HEAD"], cwd=ROOT,
                         check=True, capture_output=True).stdout.decode()
    entries = []
    for row in raw.rstrip("\0").split("\0"):
        metadata, path = row.split("\t", 1)
        mode, kind, sha = metadata.split()
        entries.append(dict(mode=mode, type=kind, sha=sha, path=path))
    subprocess.run(["git", "diff", "--exit-code", "HEAD", "--", ".",
                    *[":(exclude)" + path for path in sorted(EXCLUDED)]], cwd=ROOT,
                   check=True, stdout=subprocess.DEVNULL)
    return fingerprint(entries)


def validate_run(run, jobs):
    require(run.get("repository", {}).get("id") == REPO_ID
            and run.get("head_repository", {}).get("id") == REPO_ID, "foreign preparation")
    require(run.get("path") == ".github/workflows/release.yml"
            and run.get("event") == "workflow_dispatch" and run.get("head_branch") != "main",
            "preparation must be a branch release dry run")
    require(re.fullmatch(r"[0-9a-f]{40}", run.get("head_sha", "")), "invalid prepared source")
    require((run.get("status"), run.get("conclusion")) == ("completed", "success"),
            "preparation did not succeed")
    expected = {"slot", *["build-" + t for t in TARGETS],
                *["smoke-" + t for t in TARGETS if "linux" in t]}
    records = jobs.get("jobs", [])
    require(jobs.get("total_count") == len(records) == len(expected) + 2
            and {j.get("name") for j in records} == expected | {"publish", "distribution"},
            "incomplete preparation job inventory")
    for job in records:
        require(job.get("head_sha") == run["head_sha"] and job.get("status") == "completed"
                and job.get("conclusion") == ("success" if job["name"] in expected else "skipped"),
                "preparation job did not pass or attempted publication")


def validate_artifacts(run, inventory):
    artifacts = inventory.get("artifacts", [])
    require(inventory.get("total_count") == len(artifacts) == len(TARGETS)
            and {a.get("name") for a in artifacts} == {"theme-" + t for t in TARGETS},
            "unexpected preparation artifact inventory")
    require(len({a.get("id") for a in artifacts}) == len(TARGETS), "duplicate artifact ID")
    for artifact in artifacts:
        binding = artifact.get("workflow_run", {})
        require(binding.get("id") == run["id"] and binding.get("head_sha") == run["head_sha"]
                and binding.get("repository_id") == REPO_ID
                and binding.get("head_repository_id") == REPO_ID, "foreign artifact source")
        require(type(artifact.get("id")) is int and artifact["id"] > 0
                and artifact.get("expired") is False
                and type(artifact.get("size_in_bytes")) is int
                and 0 < artifact["size_in_bytes"] <= 4 * MAX_FILE
                and re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest", "")),
                "invalid or expired artifact")
    return sorted([{key: a[key] for key in ("id", "name", "size_in_bytes", "digest")}
                   for a in artifacts], key=lambda a: a["name"])


def unpack(archive, artifact, tag, destination):
    require(archive.stat().st_size == artifact["size_in_bytes"], "artifact size changed")
    with archive.open("rb") as stream:
        require("sha256:" + hashlib.file_digest(stream, "sha256").hexdigest() == artifact["digest"],
                "artifact container digest changed")
    target = artifact["name"].removeprefix("theme-")
    names = {"theme-" + target + ".tar.gz"}
    if "linux" in target:
        arm = target.startswith("aarch64")
        names |= {f'theme_{tag[1:]}_{"arm64" if arm else "amd64"}.deb',
                  f'theme-{tag[1:]}-1.{"aarch64" if arm else "x86_64"}.rpm'}
    digests = {}
    with zipfile.ZipFile(archive) as zipped:
        members = zipped.infolist()
        require(len(members) == len(names) and {m.filename for m in members} == names,
                "unexpected payload paths or inventory")
        for member in members:
            mode = member.external_attr >> 16
            require(not member.is_dir() and stat.S_IFMT(mode) in (0, stat.S_IFREG)
                    and 0 < member.file_size <= MAX_FILE, "invalid payload type or size")
            # Never extract paths from an archive. Only expected flat filenames
            # reach a newly-created directory, with a fixed maximum byte count.
            with zipped.open(member) as stream:
                data = stream.read(MAX_FILE + 1)
            require(len(data) == member.file_size, "payload size changed")
            digests[member.filename] = hashlib.sha256(data).hexdigest()
            (destination / member.filename).write_bytes(data)
    return digests


def collect(run_id, tag, destination, receipt=None):
    require(type(run_id) is int and run_id > 0, "invalid preparation run")
    DISTRIBUTION["asset_names"](tag)
    run = api(f"actions/runs/{run_id}")
    attempt = run.get("run_attempt")
    require(type(attempt) is int and attempt > 0 and run.get("id") == run_id, "invalid preparation attempt")
    jobs = api(f"actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100")
    validate_run(run, jobs)
    tree = api(f'git/trees/{run["head_sha"]}?recursive=1')
    require(tree.get("truncated") is False, "truncated prepared source")
    source = fingerprint(tree["tree"])
    require(local_fingerprint() == source, "source changed since preparation; dispatch a new dry run")
    artifacts = validate_artifacts(run, api(f"actions/runs/{run_id}/artifacts?per_page=100"))
    result = dict(schema=1, tag=tag, run_id=run_id, run_attempt=attempt,
                  source_sha=run["head_sha"], source_fingerprint=source, artifacts=artifacts)
    if receipt is not None:
        require({k: v for k, v in receipt.items() if k != "files"} == result,
                "prepared source, attempt or artifact binding changed")
    digests = {}
    with tempfile.TemporaryDirectory(prefix="theme-artifacts-") as temporary:
        for artifact in artifacts:
            archive = Path(temporary) / f'{artifact["id"]}.zip'
            with archive.open("wb") as stream:
                subprocess.run(["gh", "api", f'repos/snaraj/theme/actions/artifacts/{artifact["id"]}/zip'],
                               check=True, stdout=stream, timeout=180)
            digests.update(unpack(archive, artifact, tag, destination))
    require(set(digests) == DISTRIBUTION["asset_names"](tag), "missing release payload")
    if receipt is not None:
        require(digests == receipt.get("files"), "prepared payload digest changed")
    require(api(f"actions/runs/{run_id}") == run, "preparation changed during verification")
    require(validate_artifacts(run, api(f"actions/runs/{run_id}/artifacts?per_page=100")) == artifacts,
            "artifacts changed during verification")
    result["files"] = digests
    return result


def verify(destination):
    receipt = json.loads(RECEIPT.read_text())
    version = re.findall(r'^version = "([0-9]+\.[0-9]+\.[0-9]+)"$',
                         (ROOT / "Cargo.toml").read_text(), re.M)
    require(len(version) == 1 and receipt.get("tag") == "v" + version[0], "preparation version mismatch")
    result = collect(receipt.get("run_id"), receipt["tag"], destination, receipt)
    DISTRIBUTION["verify_formula"]((ROOT / "Formula/theme.rb").read_text(), result["tag"], result["files"])
    (destination / "SHA256SUMS").write_text("".join(
        f"{digest}  {name}\n" for name, digest in sorted(result["files"].items())))
    print(f'PREPARED_RELEASE=PASS {result["tag"]} run={result["run_id"]} attempt={result["run_attempt"]}')
    return result


def export_tests(destination, tag, published):
    """After byte verification, bind installed expectations to their source version."""
    require(not destination.exists(), "test destination already exists")
    require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag), "invalid test source tag")
    def git(*args):
        return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, timeout=60).stdout
    if published:
        require(api("releases/tags/" + tag).get("immutable") is True, "test source release is not immutable")
        source = api("commits/" + tag).get("sha", "")
        require(re.fullmatch(r"[0-9a-f]{40}", source), "invalid test source commit")
        git("fetch", "--no-tags", "--depth=1", "https://github.com/snaraj/theme.git", source)
        require(api("commits/" + tag).get("sha") == source, "test source tag moved")
    else:
        # The caller's verify() already bound the current tree and artifacts.
        source = git("rev-parse", "HEAD").decode().strip()
        require(re.fullmatch(r"[0-9a-f]{40}", source), "invalid prepared test source commit")
    cargo = git("show", source + ":Cargo.toml").decode()
    require(re.findall(r'^version = "([^"]+)"$', cargo, re.M) == [tag[1:]], "test source version differs")
    scripts = {}
    for name in INSTALLED_TESTS:
        path = "tests/" + name
        entry = git("ls-tree", "-z", source, "--", path).decode().rstrip("\0")
        require(re.fullmatch(r"100(?:644|755) blob [0-9a-f]{40}\t" + re.escape(path), entry),
                "missing or nonregular installed test: " + name)
        scripts[name] = git("cat-file", "blob", entry.split()[2])
        require(0 < len(scripts[name]) <= 4 * 1024 * 1024, "invalid installed test size")
    destination.mkdir(parents=True)
    for name, data in scripts.items():
        (destination / name).write_bytes(data)
    (destination / "manifest.json").write_text(json.dumps(dict(tag=tag, source_sha=source,
        published=published, scripts={name: hashlib.sha256(data).hexdigest() for name, data in scripts.items()}),
        indent=2, sort_keys=True) + "\n")
    print(f"INSTALLED_TEST_SOURCE=PASS {tag} source={source}")


def homebrew(tests_output=None):
    """Published releases use normal URLs; an unpublished version uses verified cache bytes."""
    formula = (ROOT / "Formula/theme.rb").read_text()
    versions = re.findall(r'^  version "([0-9]+\.[0-9]+\.[0-9]+)"$', formula, re.M)
    require(len(versions) == 1, "invalid formula version")
    tag = "v" + versions[0]
    release = subprocess.run(["gh", "api", "repos/snaraj/theme/releases/tags/" + tag],
                             capture_output=True, text=True, timeout=30)
    if release.returncode == 0:
        subprocess.run(["python3", "-I", "-B", str(ROOT / ".github/scripts/verify_distribution.py")], check=True)
        if tests_output is not None:
            export_tests(tests_output, tag, True)
        return
    require("(HTTP 404)" in release.stderr, "cannot determine publication status: " + release.stderr)
    with tempfile.TemporaryDirectory(prefix="theme-homebrew-") as temporary:
        destination = Path(temporary)
        verify(destination)
        cache = Path(subprocess.run(["brew", "--cache", "--formula", "snaraj/theme/theme"],
                                   capture_output=True, text=True, check=True).stdout.strip())
        matches = [p for p in destination.glob("*.tar.gz") if cache.name.endswith("--" + p.name)]
        require(len(matches) == 1 and cache.is_absolute(), "unexpected Homebrew cache path")
        cache.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(matches[0], cache)
        if tests_output is not None:
            export_tests(tests_output, tag, False)


def install_tar(archive, destination):
    """Read one bounded regular executable, never extract archive paths."""
    with tarfile.open(archive, "r:gz") as tar:
        member = tar.next()
        require(member is not None and member.name == "theme" and member.isfile()
                and 0 < member.size <= MAX_FILE, "invalid release executable")
        data = tar.extractfile(member).read(MAX_FILE + 1)
        require(len(data) == member.size and tar.next() is None, "unexpected release contents")
    destination.mkdir(parents=True)
    binary = destination / "theme"
    binary.write_bytes(data)
    binary.chmod(0o755)
    return binary


def install(target, destination, tests_output=None):
    formula = (ROOT / "Formula/theme.rb").read_text()
    versions = re.findall(r'^  version "([0-9]+\.[0-9]+\.[0-9]+)"$', formula, re.M)
    require(len(versions) == 1 and target in TARGETS, "invalid release installation")
    tag = "v" + versions[0]
    release = subprocess.run(["gh", "api", "repos/snaraj/theme/releases/tags/" + tag],
                             capture_output=True, text=True, timeout=30)
    with tempfile.TemporaryDirectory(prefix="theme-install-") as temporary:
        directory = Path(temporary)
        if release.returncode == 0:
            DISTRIBUTION["collect"](tag, formula, directory)
        else:
            require("(HTTP 404)" in release.stderr, "cannot determine publication status: " + release.stderr)
            verify(directory)
        binary = install_tar(directory / f"theme-{target}.tar.gz", destination)
    version = subprocess.run([str(binary), "-V"], check=True, capture_output=True, text=True).stdout
    require(version.splitlines()[:1] == [f"version: {tag}"], "installed release version differs")
    if tests_output is not None:
        export_tests(tests_output, tag, release.returncode == 0)
    print(f"INSTALLED_RELEASE=PASS {tag} binary_sha256={hashlib.sha256(binary.read_bytes()).hexdigest()}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("record", "verify", "homebrew", "install"))
    parser.add_argument("--run", type=int)
    parser.add_argument("--tag")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--tests-output", type=Path)
    args = parser.parse_args()
    if args.mode == "homebrew":
        homebrew(args.tests_output)
    elif args.mode == "install":
        require(args.target and args.output and not args.output.exists(), "install needs --target and a new --output")
        install(args.target, args.output.resolve(), args.tests_output)
    elif args.mode == "record":
        require(args.run and args.tag and args.output, "record needs --run, --tag and --output")
        with tempfile.TemporaryDirectory(prefix="theme-preparation-") as temporary:
            receipt = collect(args.run, args.tag, Path(temporary))
        args.output.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    else:
        require(args.output and not args.output.exists(), "verify needs a new --output directory")
        args.output.mkdir(parents=True)
        verify(args.output)


if __name__ == "__main__":
    main()
