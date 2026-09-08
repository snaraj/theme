"""Verify downloadable release bytes and the four digest-pinned Homebrew routes."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

REPOSITORY = "snaraj/theme"
TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin",
           "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu")
ROOT = Path(__file__).resolve().parents[2]


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def api(path):
    return json.loads(subprocess.run(["gh", "api", f"repos/{REPOSITORY}/{path}"],
                                    check=True, capture_output=True, text=True, timeout=30).stdout)


def asset_names(tag):
    require(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag), "invalid release tag")
    version = tag[1:]
    return {f"theme-{t}.tar.gz" for t in TARGETS} | {
        f"theme_{version}_{a}.deb" for a in ("amd64", "arm64")} | {
        f"theme-{version}-1.{a}.rpm" for a in ("x86_64", "aarch64")}


def inventory(release, tag):
    names = asset_names(tag)
    require(release.get("tag_name") == tag and release.get("draft") is False
            and release.get("prerelease") is False, "release is not published and stable")
    assets = release.get("assets", [])
    require(len(assets) == len(names) + 1 and {a.get("name") for a in assets} == names | {"SHA256SUMS"},
            "missing, duplicate or unexpected published asset")
    for asset in assets:
        name = asset["name"]
        size = asset.get("size")
        require(asset.get("state") == "uploaded" and type(size) is int and 0 < size <= 64 * 1024 * 1024,
                "invalid published asset size or upload state")
    return assets


def snapshot(release):
    return ({k: release.get(k) for k in ("id", "tag_name", "draft", "prerelease", "immutable")},
            [{k: a.get(k) for k in ("id", "name", "size", "digest", "state")} for a in release.get("assets", [])])


def verify_bytes(release, tag, directory):
    names = asset_names(tag)
    digests = {}
    for asset in inventory(release, tag):
        name, size = asset["name"], asset["size"]
        path = directory / name
        require(path.is_file() and not path.is_symlink() and path.stat().st_size == size,
                f"download missing or wrong size: {name}")
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        require(asset.get("digest") == "sha256:" + digest, f"GitHub digest mismatch: {name}")
        digests[name] = digest
    sums = (directory / "SHA256SUMS").read_text()
    expected = {f"{digests[name]}  {name}" for name in names}
    require(len(sums.splitlines()) == len(names) and set(sums.splitlines()) == expected,
            "SHA256SUMS does not exactly match every downloaded package")
    return digests


def verify_formula(formula, tag, digests):
    require(re.findall(r'^  version "([^"]+)"$', formula, re.M) == [tag[1:]],
            f"Homebrew is pending: bump Formula/theme.rb to {tag} with verified release digests")
    routes = re.findall(r'    on_(arm|intel) do\n      url "([^"]+)"\n      sha256 "([0-9a-f]{64})"', formula)
    expected = [("arm" if t.startswith("aarch64") else "intel",
                 f"https://github.com/{REPOSITORY}/releases/download/{tag}/theme-{t}.tar.gz",
                 digests[f"theme-{t}.tar.gz"]) for t in TARGETS]
    require(routes == expected, "Homebrew routes or checksums differ from the verified release")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="require this published release; defaults to the formula version")
    args = parser.parse_args()
    formula = (ROOT / "Formula/theme.rb").read_text()
    versions = re.findall(r'^  version "([0-9]+\.[0-9]+\.[0-9]+)"$', formula, re.M)
    require(len(versions) == 1, "formula must declare exactly one stable version")
    tag = args.tag or "v" + versions[0]
    names = asset_names(tag) | {"SHA256SUMS"}
    release = api("releases/tags/" + tag)
    inventory(release, tag)
    with tempfile.TemporaryDirectory(prefix="theme-distribution-") as directory:
        command = ["gh", "release", "download", tag, "--repo", REPOSITORY, "--dir", directory]
        for name in sorted(names):
            command += ["--pattern", name]
        subprocess.run(command, check=True, timeout=180)
        digests = verify_bytes(release, tag, Path(directory))
    require(snapshot(api("releases/tags/" + tag)) == snapshot(release), "release changed during verification")
    print(f"PUBLISHED_ASSETS=PASS tag={tag} assets={len(names)}", flush=True)
    verify_formula(formula, tag, digests)
    print(f"HOMEBREW_DISTRIBUTION=PASS tag={tag}")


if __name__ == "__main__":
    main()
