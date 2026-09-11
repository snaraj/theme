#!/usr/bin/env python3
"""macOS provenance batching/cache contract against the actual theme CLI.

Only index/list/search are allowed. Cache writes stay in private target fixtures;
all image bytes, source attributes and mdls replies are synthetic.
"""

import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def png(path, color):
    def chunk(kind, data):
        return (struct.pack("!I", len(data)) + kind + data
                + struct.pack("!I", zlib.crc32(kind + data) & 0xFFFFFFFF))
    rows = b"".join(b"\0" + bytes(color) * 32 for _ in range(24))
    path.write_bytes(b"\x89PNG\r\n\x1a\n"
                     + chunk(b"IHDR", struct.pack("!2I5B", 32, 24, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


MDLS = r'''
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
with open(os.environ["TEST_MDLS_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps(args) + "\n")
if args[:3] != ["-raw", "-name", "kMDItemWhereFroms"] or len(args) < 4:
    sys.exit(3)
labels = json.loads(os.environ["TEST_MDLS_LABELS"])
fields = [('(\n    "https://' + labels[Path(path).name] + '/image"\n)').encode()
          for path in args[3:]]
mode = os.environ["TEST_MDLS_MODE"]
if len(fields) > 1:
    if mode == "nonzero":
        sys.stdout.buffer.write(b"\0".join(fields))
        sys.exit(1)
    if mode == "wrong-count":
        fields = fields[:1]
    if mode == "oversized":
        try:
            sys.stdout.buffer.write(b"x" * (256 * 1024 + 1))
            sys.stdout.buffer.flush()
        except BrokenPipeError:
            pass
        sys.exit(0)
sys.stdout.buffer.write(b"\0".join(fields))
'''


def main():
    require(len(sys.argv) <= 2, "usage: mdls_cli_test.py [theme-binary]")
    if sys.platform != "darwin":
        print("mdls CLI: SKIP (macOS only)")
        return
    repo = Path(__file__).resolve().parents[1]
    binary = Path(sys.argv[1] if len(sys.argv) == 2 else
                  os.environ.get("THEME_BIN", repo / "target/release/theme")).resolve()
    require(binary.is_file(), f"theme binary is missing: {binary}")
    target = repo / "target"
    target.mkdir(exist_ok=True)
    fixture = Path(tempfile.mkdtemp(prefix="mdls-cli-", dir=target))
    try:
        dirs = {name: fixture / name for name in ("library", "config", "kitty", "bin", "tmp")}
        for directory in dirs.values():
            directory.mkdir(mode=0o700)
        labels = {"amber sample.png": "qxvhost", "fern.png": "qjkhost", "river.png": "qzfhost"}
        paths = [dirs["library"] / name for name in labels]
        for path, color in zip(paths, ((44, 65, 110), (60, 90, 30), (30, 95, 150))):
            png(path, color)
        settings = dirs["kitty"] / "kitty.conf"
        settings.write_text("background_opacity 1\n", encoding="utf-8")
        fake = dirs["bin"] / "mdls"
        fake.write_text(f"#!{sys.executable}\n" + MDLS, encoding="utf-8")
        fake.chmod(0o700)
        log = fixture / "mdls.jsonl"
        # Preserve the real HOME if present, never redirect it. Every theme
        # configuration/cache/library path is explicitly fixture-owned.
        env = {key: os.environ[key] for key in ("HOME", "USER", "LOGNAME") if key in os.environ}
        env.update(PATH=f"{dirs['bin']}:/usr/bin:/bin", TMPDIR=str(dirs["tmp"]),
                   CONFIG_DIR=str(dirs["config"]), KITTY_CONFIG_DIRECTORY=str(dirs["kitty"]),
                   THEME_WALLPAPER_DIR=str(dirs["library"]), THEME_CACHE_DIR=str(fixture / "cache"),
                   THEME_NO_UPDATE_CHECK="1", THEME_OPACITY="1", THEME_CONTRAST="4.5",
                   THEME_FORMATS="png", THEME_EXCLUDE_FORMATS="", COLUMNS="240",
                   TERM="xterm-256color", TZ="UTC", LANG="C", LC_ALL="C",
                   TEST_MDLS_LOG=str(log), TEST_MDLS_LABELS=json.dumps(labels), TEST_MDLS_MODE="good")

        def run(args, mode="good", cache="cache"):
            require(args and args[0] in ("list", "search", "index"), "fixture command outside read-only allowlist")
            log.write_text("", encoding="utf-8")
            result = subprocess.run([str(binary), *args], input=b"", stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, timeout=20,
                                    env=env | {"TEST_MDLS_MODE": mode, "THEME_CACHE_DIR": str(fixture / cache)})
            require(result.returncode == 0, f"{args}: {result.returncode}: {result.stderr!r}")
            calls = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
            require(all(call[:3] == ["-raw", "-name", "kMDItemWhereFroms"] for call in calls),
                    f"unexpected mdls arguments: {calls!r}")
            return result.stdout, [call[3:] for call in calls]

        def mapped(output, expected):
            text = re.sub(rb"\x1b\[[0-9;:]*m", b"", output).decode("utf-8")
            for path in paths:
                rows = [line for line in text.splitlines() if line.startswith("  " + path.stem + " ")]
                require(len(rows) == 1, f"missing/duplicate row for {path.name}: {text!r}")
                # COLUMNS=240 fixes TITLE at 44 cells and COLORSCHEME at 24.
                require(rows[0][74:84].strip() == expected[path.name],
                        f"wrong source mapping for {path.name}: {rows[0]!r}")

        run(["index"])  # Exclude one-time palette derivation notes from the byte oracle.
        baseline, calls = run(["list", "-v", "--all"])
        mapped(baseline, labels)
        require(len(calls) == 1 and len(calls[0]) == len(paths)
                and set(calls[0]) == {str(path) for path in paths}, f"missing full batch: {calls!r}")
        batch = calls[0]
        for mode in ("nonzero", "wrong-count", "oversized"):
            output, calls = run(["list", "-v", "--all"], mode)
            require(output == baseline, f"{mode} fallback changed per-file source output")
            require(calls == [batch] + [[path] for path in batch],
                    f"{mode} must fall back once per requested file: {calls!r}")

        indexed, calls = run(["index"], cache="index-cache")
        require(b"cached 3 of 3 metadata record(s)" in indexed, f"index not persisted: {indexed!r}")
        require(len(calls) == 1 and set(calls[0]) == set(batch), f"cold index did not batch: {calls!r}")
        for path in paths:
            label = labels[path.name]
            output, calls = run(["search", label, "--all"], cache="index-cache")
            require(not calls, f"warm source-only search queried mdls: {calls!r}")
            require(path.stem.encode() in output and f"source: {label}".encode() in output
                    and b"1 of 3 wallpapers match" in output,
                    f"warm source-only search lost its exact match: {output!r}")

        changed = paths[0]
        png(changed, (75, 35, 160))
        output, calls = run(["search", labels[changed.name], "--all"], cache="index-cache")
        require(calls == [[str(changed)]], f"identity change queried unrelated paths: {calls!r}")
        require(f"source: {labels[changed.name]}".encode() in output, "changed file lost provenance")

        # Only this synthetic file receives a source xattr. The mdls fixture
        # deliberately disagrees, so precedence cannot pass accidentally.
        subprocess.run(["/usr/bin/xattr", "-w", "theme.source", "https://qxattr/fixture", str(changed)],
                       check=True, capture_output=True, timeout=10, env=env)
        output, calls = run(["list", "-v", "--all"])
        mapped(output, labels | {changed.name: "qxattr"})
        require(len(calls) == 1 and len(calls[0]) == 2
                and set(calls[0]) == {str(path) for path in paths[1:]},
                f"source xattr did not exclude its path from mdls: {calls!r}")
        require(settings.read_text(encoding="utf-8") == "background_opacity 1\n"
                and list(dirs["config"].iterdir()) == []
                and list(dirs["kitty"].iterdir()) == [settings], "fixture settings changed")
        print("mdls CLI: batch mapping, failure/size fallbacks, xattr precedence and index reuse PASS")
    finally:
        shutil.rmtree(fixture)


if __name__ == "__main__":
    main()
