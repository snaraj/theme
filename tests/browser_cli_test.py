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


def pty_available():
    """A pty is the only way to test a terminal reader; without one the
    three flows below are skipped, and CI refuses that skip."""
    try:
        import pty
        master, slave = pty.openpty()
    except (ImportError, OSError) as error:
        print(f"browser CLI: PTY unavailable ({type(error).__name__}: {error}); skipped")
        return False
    os.close(master)
    os.close(slave)
    return True


class Terminal:
    """One pty-driven browser session: press keys, wait for what appears.

    A real person reaches for keys, so the test does too — the bytes written
    here are the bytes a terminal sends. Waiting is always for output the
    browser has not printed yet: a page counter from two keys ago must never
    answer for this key, so the read mark only moves forward.
    """

    def __init__(self, binary, env, argv, stdout=None, columns=25):
        import fcntl
        import pty
        import termios
        self.termios = termios
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, columns, 0, 0))
        self.cooked = termios.tcgetattr(self.master)
        self.output, self.mark = bytearray(), 0
        self.process = subprocess.Popen([str(binary), *argv], env=env, stdin=slave,
                                        stdout=slave if stdout is None else stdout, stderr=slave)
        os.close(slave)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        os.close(self.master)
        return False

    def drain(self, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if not select.select([self.master], [], [], 0.05)[0]:
                continue
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    return  # the child closed the pty: it has left
                raise
            if not data:
                return
            self.output.extend(data)
            require(len(self.output) < 1 << 20, "PTY output exceeded its bound")

    def send(self, data):
        os.write(self.master, data)

    def expect(self, wanted, seconds=15):
        deadline = time.monotonic() + seconds
        while True:
            found = bytes(self.output[self.mark:]).find(wanted.encode())
            if found >= 0:
                self.mark += found + len(wanted)
                return
            require(time.monotonic() < deadline,
                    f"{wanted!r} never appeared; screen was {self.screen()[-1500:]!r}")
            self.drain(0.2)

    def press(self, keys, wanted):
        self.send(keys)
        self.expect(wanted)

    def screen(self, start=0):
        """Everything printed, colours removed — the layout oracle's input."""
        return re.sub(rb"\x1b\[[0-9;:]*m", b"", bytes(self.output[start:])).decode("utf-8", "replace")

    def raw_mode(self):
        """Whether the terminal is in raw mode right now (echo off, no line
        discipline). The master and slave share one termios."""
        modes = self.termios.tcgetattr(self.master)[3]
        return not modes & self.termios.ECHO and not modes & self.termios.ICANON

    def leave(self, keys=b"q\n", code=0):
        self.send(keys)
        deadline = time.monotonic() + 15
        while self.process.poll() is None:
            # Keep reading while it goes: a pty whose master stops being
            # read blocks the child mid-write, which is not an exit path.
            require(time.monotonic() < deadline,
                    f"the browser did not leave: {self.screen()[-1500:]!r}")
            self.drain(0.2)
        require(self.process.returncode == code,
                f"exit {self.process.returncode}: {self.screen()[-2000:]!r}")
        self.drain(0.3)
        require(self.termios.tcgetattr(self.master) == self.cooked,
                "the terminal was not given back the mode it was found in")


def pty_navigation(binary, env):
    """The typed-command flow, unchanged: every word still reaches its
    command now that the reader is a key reader."""
    with Terminal(binary, env, ["browse", "--all"]) as term:
        term.expect("page 1/1")
        require(term.raw_mode(), "an interactive browser did not enter raw mode")
        for command in [b"next", b"favorite", b"next", b"prev", b"page 1", b"history",
                        b"shuffle", b"next", b"select 4", b"apply"]:
            term.expect("browse> ")
            term.press(command + b"\n", command.decode())
        term.leave(b"quit\n")
        text = term.screen()
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


def pty_keys(binary, env):
    """The path a person actually takes: arrows, PageUp/PageDown, Space,
    Home/End, then the one-letter aliases. One image per page makes the page
    counter the assertion for every key."""
    with Terminal(binary, env, ["browse", "--all", "--page-size", "1"]) as term:
        term.expect("page 1/4")
        # The title line names the picture the selection is on: only a
        # preview prints it, so it is what proves the selection moved.
        for keys, wanted in [(b"\x1b[C", "1 blue calm 01.png"), (b"\x1b[B", "page 2/4"),
                             (b"\x1b[6~", "page 3/4"), (b"\x1b[5~", "page 2/4"),
                             (b" ", "page 3/4"), (b"\x1b[H", "page 1/4"),
                             (b"\x1b[F", "page 4/4"), (b"\x1bOC", "2 blue calm 02.png"),
                             (b"\x1bOD", "1 blue calm 01.png"), (b"n\n", "2 blue calm 02.png"),
                             (b"p\n", "1 blue calm 01.png"), (b"?\n", "theme browse [terms...]")]:
            term.press(keys, wanted)
        require("Preview only" in term.screen(), "the arrow keys never previewed")
        # Both ends are ends: a note, no wrap, no crash.
        term.press(b"\x1b[H", "page 1/4")
        term.press(b"\x1b[5~", "start of these")
        term.press(b"\x1b[F", "page 4/4")
        term.press(b"\x1b[6~", "end of these")
        require(all(cells(line) <= 25 for line in term.screen().split("[no-apply]")[0].splitlines()),
                f"key-driven browser exceeds 25 columns: {term.screen()[-2000:]!r}")
        term.leave()


def pty_boundaries(binary, env, piped_page):
    """One test per trust boundary, each driven with its hostile case."""
    # A pipe on stdout is not a terminal: no raw mode, no keys read, and
    # byte-for-byte the page a fully redirected run prints.
    with Terminal(binary, env, ["browse", "--all"], stdout=subprocess.PIPE) as term:
        require(term.process.communicate(timeout=15)[0] == piped_page,
                "a piped stdout changed the deterministic page")
        require(term.termios.tcgetattr(term.master) == term.cooked,
                "raw mode was entered although stdout was a pipe")
        require(term.process.returncode == 0, f"piped browse exited {term.process.returncode}")
    # The terminal mode comes back on every way out, not just the tidy one.
    for keys in (b"q\n", b"\x04", b"\x03"):
        with Terminal(binary, env, ["browse", "--all"]) as term:
            term.expect("page 1/1")
            require(term.raw_mode(), "an interactive browser did not enter raw mode")
            term.leave(keys)
    # 2000 bytes of never-finished escape prefix: bounded, silent, alive.
    with Terminal(binary, env, ["browse", "--all", "--page-size", "1"]) as term:
        term.expect("page 1/4")
        term.send(b"\x1b[" * 1000)
        term.drain(1.0)  # longer than the browser's own wait for a lone Escape
        require("page 2/4" not in term.screen(), "escape garbage moved the browser")
        require("would" not in term.screen(), "escape garbage reached apply")
        term.leave()
    with Terminal(binary, env, ["browse", "--all", "--page-size", "1"]) as term:
        term.expect("page 1/4")
        # A pasted control byte is dropped, and the line it poisoned is
        # refused whole rather than run with the byte quietly removed.
        term.press(b"sel\x1bect 1\n", "command contains")
        require("Preview only" not in term.screen(), "a pasted control line was executed")
        # An arrow mid-command navigates nothing; the line completes as typed.
        term.send(b"sel\x1b[C")
        term.drain(0.4)
        require("page 2/4" not in term.screen() and "Preview only" not in term.screen(),
                "an arrow navigated while a command was being typed")
        term.press(b"ect 1\n", "Preview only")
        # Space mid-command is a space, not a page turn.
        term.press(b"page 3\n", "page 3/4")
        # Invalid UTF-8 in a line is a bad command, never a panic.
        term.press(b"sel\xff\xfeect 1\n", "unknown command")
        term.leave()


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
        pty_ok = pty_available()
        if os.environ.get("CI") == "true":
            require(pty_ok, "CI must exercise the synthetic terminal, not skip it")
        if pty_ok:
            pty_navigation(binary, env)
            pty_keys(binary, env)
            pty_boundaries(binary, env, eof)
        after = {p.relative_to(fixture): p.read_bytes() for p in fixture.rglob("*") if p.is_file()}
        require(before == after and not (fixture / "cache").exists(),
                "NO_APPLY navigation changed fixture settings or cache")
        print("browser CLI: dispatch, filters, width, non-TTY and settings PASS; "
              + ("PTY commands, keys and boundaries PASS" if pty_ok else "PTY flows SKIP"))
    finally:
        shutil.rmtree(fixture)


if __name__ == "__main__":
    main()
