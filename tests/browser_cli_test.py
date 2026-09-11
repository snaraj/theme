#!/usr/bin/env python3
"""Executable browser contract using synthetic files and THEME_NO_APPLY only."""

import errno
import os
from pathlib import Path
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import unicodedata
import zlib


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def cells(text):
    """Independent width oracle for our ASCII, CJK, combining and PUA fixtures."""
    return sum(0 if unicodedata.category(c) in ("Mn", "Me", "Cf") else
               2 if unicodedata.east_asian_width(c) in ("W", "F") else 1 for c in text)


def png(path, color):
    def chunk(kind, data):
        return (struct.pack("!I", len(data)) + kind + data
                + struct.pack("!I", zlib.crc32(kind + data) & 0xFFFFFFFF))
    rows = b"".join(b"\0" + bytes(color) * 64 for _ in range(48))
    path.write_bytes(b"\x89PNG\r\n\x1a\n"
                     + chunk(b"IHDR", struct.pack("!2I5B", 64, 48, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def pty_navigation(binary, env):
    try:
        import fcntl
        import pty
        import termios
        master, slave = pty.openpty()
    except (ImportError, OSError) as error:
        print(f"browser CLI: PTY unavailable ({type(error).__name__}); skipped")
        return False
    process = None
    output = bytearray()
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 25, 0, 0))
        process = subprocess.Popen([str(binary), "browse", "--all"], env=env,
                                   stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        slave = None
        deadline = time.monotonic() + 20
        commands = [b"next", b"favorite", b"next", b"prev", b"page 1", b"history",
                    b"shuffle", b"next", b"select 4", b"apply", b"quit"]
        sent = 0
        while time.monotonic() < deadline:
            if not select.select([master], [], [], 0.1)[0]:
                if process.poll() is not None:
                    break
                continue
            try:
                data = os.read(master, 8192)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not data:
                break
            output.extend(data)
            require(len(output) < 262144, "PTY output exceeded its bound")
            if sent < len(commands) and output.count(b"browse> ") > sent:
                os.write(master, commands[sent] + b"\n")
                sent += 1
        require(sent == len(commands), "PTY did not complete the navigation session")
        require(process.wait(timeout=2) == 0, repr(bytes(output[-3000:])))
        text = re.sub(rb"\x1b\[[0-9;:]*m", b"", bytes(output)).decode("utf-8")
        flat = " ".join(text.split())
        # Browser navigation/specimens must fit. The existing shared dry-run
        # apply announcement prints a full path and is outside browser layout.
        preview = text.split("[no-apply]", 1)[0]
        require(all(cells(line) <= 25 for line in preview.splitlines()),
                f"interactive preview exceeds 25 columns: {preview!r}")
        for wanted in ["Sampled readability:", "saved favorite", "Preview only.",
                       "[no-apply] would set the desktop wallpaper"]:
            require(wanted in flat, f"missing interactive result {wanted!r}: {flat[-3000:]!r}")
        require("unknown command" not in flat, "navigation command was rejected")
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=2)
        os.close(master)
        if slave is not None:
            os.close(slave)
    return True


def main():
    require(len(sys.argv) <= 2, "usage: browser_cli_test.py [theme-binary]")
    repo = Path(__file__).resolve().parents[1]
    binary = Path(sys.argv[1] if len(sys.argv) == 2 else
                  os.environ.get("THEME_BIN", repo / "target/release/theme")).resolve()
    require(binary.is_file(), f"theme binary is missing: {binary}")
    target = repo / "target"
    target.mkdir(exist_ok=True)
    fixture = Path(tempfile.mkdtemp(prefix="browser-cli-", dir=target))
    try:
        library, config, kitty = (fixture / name for name in ("library", "config", "kitty"))
        for directory in (library, config, kitty):
            directory.mkdir(mode=0o700)
        for name, color in [("blue calm 01.png", (24, 45, 120)),
                            ("blue calm 02.png", (36, 60, 135)),
                            ("green hills #3.png", (30, 95, 45)),
                            ("界" * 20 + ".png", (40, 90, 120))]:
            png(library / name, color)
        (kitty / "kitty.conf").write_text("background_opacity 0.8\n")
        before = {p.relative_to(fixture): p.read_bytes() for p in fixture.rglob("*") if p.is_file()}
        env = os.environ.copy()  # HOME is preserved, never redirected.
        for name in ("KITTY_WINDOW_ID", "KITTY_LISTEN_ON", "LISTEN_ON"):
            env.pop(name, None)
        env.update(CONFIG_DIR=str(config), KITTY_CONFIG_DIRECTORY=str(kitty),
                   THEME_CACHE_DIR=str(fixture / "cache"), THEME_WALLPAPER_DIR=str(library),
                   THEME_NO_APPLY="1", THEME_NO_UPDATE_CHECK="1", THEME_OPACITY="0.8",
                   THEME_CONTRAST="7", COLUMNS="25", THEME_FORMATS="png", THEME_EXCLUDE_FORMATS="")

        def run(args, code=0, stdin=b"", overrides=None):
            child_env = env | (overrides or {})
            result = subprocess.run([str(binary), *args], input=stdin, env=child_env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
            require(result.returncode == code,
                    f"{args!r}: exit {result.returncode}, {result.stderr[-2000:]!r}")
            return result.stdout, result.stderr

        # Both aliases and both command-help routes reach the same browser.
        eof, _ = run(["browse", "--all"])
        piped, _ = run(["surf", "--all"], stdin=b"select 1\napply\nquit\n")
        require(eof == piped, "non-TTY input was interpreted as browser commands")
        require(b"would" not in piped and b"Sampled readability" not in piped,
                "non-TTY browse entered selection/apply")
        help_text, _ = run(["browse", "--help"])
        for args in (["surf", "--help"], ["help", "browse"], ["help", "surf"]):
            require(run(args)[0] == help_text, f"different help dispatch: {args}")
        require(b"theme browse" in help_text, "missing browser help")
        for text in (eof, help_text):
            require(b"\x1b" not in text, "plain output contains terminal sequences")
            require(all(cells(line) <= 25 for line in text.decode().splitlines()),
                    f"output exceeds 25 columns: {text!r}")
        wide, _ = run(["browse", "--all"], overrides={"COLUMNS": "240"})
        require(b"blue calm 01.png" in wide and b"green hills #3.png" in wide
                and ("界" * 20 + ".png").encode() in wide,
                "filenames with spaces lost their spelling")
        for width in (1, 8, 13):
            narrow, _ = run(["browse", "--all"], overrides={"COLUMNS": str(width)})
            require(all(cells(line) <= width for line in narrow.decode().splitlines()),
                    f"Unicode filenames exceed {width} columns")
        valid, _ = run(["browse", "--all", "--min-width", "32", "--min-height", "24",
                        "--aspect", "4:3", "--min-contrast", "1", "--coverage", "0"])
        require("4 matches" in " ".join(valid.decode().split()),
                f"valid measured filters lost fixture images: {valid!r}")
        require(all(cells(line) <= 25 for line in valid.decode().splitlines()),
                "measured-filter prose exceeds 25 columns")
        for options in (["--unknown"], ["--page-size", "0"], ["--page-size", "25"],
                        ["--page-size", "1.5"], ["--color", "invisible"],
                        ["--min-width", "0"], ["--min-height", "-1"], ["--aspect", "16:0"],
                        ["--min-contrast", "NaN"], ["--min-contrast", "22"],
                        ["--coverage", "1.1"], ["--coverage", "NaN"], ["--color"]):
            out, _ = run(["browse", *options], code=1, stdin=b"apply\n")
            require(b"would" not in out, f"invalid filters reached apply: {options}")
        for contrast in ("", "NaN", "inf", "0", "21.1", "-1"):
            out, err = run(["browse", "--all"], code=1, overrides={"THEME_CONTRAST": contrast})
            require(b"CONTRAST" in err and b"would" not in out, "invalid contrast was not refused")
        # A malformed opacity must stop the shared apply path before even the
        # dry-run desktop announcement; all test applies remain NO_APPLY.
        for opacity in ("", "NaN", "-0.1", "1.1"):
            out, err = run(["set", str(library / "blue calm 01.png")], code=1,
                           overrides={"THEME_OPACITY": opacity})
            require(b"opacity" in err.lower() and b"would" not in out,
                    f"opacity refusal came after apply: {out!r} {err!r}")
        pty_ok = pty_navigation(binary, env)
        if os.environ.get("CI") == "true":
            require(pty_ok, "CI must exercise the synthetic terminal, not skip it")
        after = {p.relative_to(fixture): p.read_bytes() for p in fixture.rglob("*") if p.is_file()}
        require(before == after and not (fixture / "cache").exists(),
                "NO_APPLY navigation changed fixture settings or cache")
        print("browser CLI: dispatch, filters, width, non-TTY and settings PASS; "
              + ("PTY navigation PASS" if pty_ok else "PTY navigation SKIP"))
    finally:
        shutil.rmtree(fixture)


if __name__ == "__main__":
    main()
