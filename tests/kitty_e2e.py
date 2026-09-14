#!/usr/bin/env python3
"""The browser's key path, driven in a real Kitty and read back off the screen.

The defect this exists for reached a published release because every browser
test typed whole words down a pipe: nothing ever pressed a key in a terminal
that draws. So this driver launches a pinned Kitty, runs `theme browse` inside
it, sends the bytes a keyboard sends through `kitten @ send-text`, and asserts
on what the screen says afterwards — plus a screenshot, decoded here, proving
the thumbnails are pictures rather than empty rows.

Usage:
  python3 -I -B tests/kitty_e2e.py --kitty <kitty binary> --theme <theme binary>
                                   --output <empty directory>

Exit 0 when every assertion passed, 1 otherwise. Every screenshot, screen dump
and kitty log is written under --output, pass or fail, and summary.json records
the geometry and the numbers each assertion measured.

What it does NOT prove: the desktop side. Applying a wallpaper, macOS Spaces
and the palette delivered to a second window are outside this test — it never
presses `apply`, and THEME_NO_APPLY is set so it could not if it tried.
"""

import argparse
import json
import importlib.util
import hashlib
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib

# -I deliberately omits the script directory from sys.path.
_spec = importlib.util.spec_from_file_location("kitty_pixels", Path(__file__).with_name("kitty_pixels.py"))
picture = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(picture)

IMAGES = 14
PAGE_SIZE = 6  # 14 images over 6 per page: three pages, the last one short
PAGES = -(-IMAGES // PAGE_SIZE)
WINDOW = (1400, 900)
THUMBNAIL_ROWS = 7  # what the sheet reserves per card, before title and swatches
PLACEHOLDER = "\U0010eeee"  # the cell kitty fills with a transmitted image
PROMPT = "browse>"  # printed last, which is what makes it the settled signal
SOCKET_WAIT = 30.0
ASSERT_WAIT = 10.0
POLL = 0.2


class Failure(Exception):
    """An assertion that did not hold, with the screen behind it."""


def decode_png(data, max_rows=None):
    """Enough of a PNG reader for what the two screenshot tools write: 8-bit
    RGB or RGBA, no interlacing, filters 0-4. Decoding stops after `max_rows`
    scanlines — the rows this test reads are near the top, and a Retina
    capture is 20 MB that nothing here needs whole."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise Failure("screenshot is not a PNG")
    position, parts, header = 8, [], None
    while position + 8 <= len(data):
        length = struct.unpack(">I", data[position:position + 4])[0]
        kind = data[position + 4:position + 8]
        body = data[position + 8:position + 8 + length]
        position += 12 + length
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            parts.append(body)
        elif kind == b"IEND":
            break
    if header is None:
        raise Failure("screenshot has no IHDR")
    width, height, depth, colour, compression, filters, interlace = header
    if (depth, compression, filters, interlace) != (8, 0, 0, 0) or colour not in (2, 6):
        raise Failure(f"unsupported screenshot encoding: IHDR={header}")
    channels = 3 if colour == 2 else 4
    rows = height if max_rows is None else min(height, max_rows)
    raw = zlib.decompress(b"".join(parts))
    stride = width * channels
    pixels = bytearray()
    prior = bytearray(stride)
    at = 0
    for _ in range(rows):
        kind, at = raw[at], at + 1
        line = bytearray(raw[at:at + stride])
        at += stride
        if kind == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif kind == 2:
            for i in range(stride):
                line[i] = (line[i] + prior[i]) & 0xFF
        elif kind == 3:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + prior[i]) >> 1)) & 0xFF
        elif kind == 4:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prior[i]
                c = prior[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 0xFF
        elif kind != 0:
            raise Failure(f"unknown PNG row filter {kind}")
        pixels.extend(line)
        prior = line
    return {"width": width, "height": height, "channels": channels,
            "rows": rows, "pixels": bytes(pixels)}


class Fixture:
    """A private library, cache, config and PATH: this test reads and writes
    nothing of the machine it runs on."""

    def __init__(self, theme, kitty_bin, directory):
        # Saving rejects world-writable ancestors, including Linux /tmp.
        self.root = Path(tempfile.mkdtemp(prefix="fixture-", dir=directory))
        # UNIX sockets have a short path limit; keep the controller separate
        # from image storage so deeply nested checkouts remain usable.
        self.socket_dir = Path(tempfile.mkdtemp(prefix="theme-socket-"))
        self.library = self.root / "library"
        for child in ("library", "cache", "config/kitty", "kitty-config", "tmp", "bin"):
            (self.root / child).mkdir(mode=0o700, parents=True)
        self.sizes = {}
        for i in range(IMAGES):
            # Distinct picture sizes exercise fitting throughout navigation.
            size = ((360, 240), (240, 360), (480, 160))[i % 3]
            (self.library / f"fixture-{i:03d}.png").write_bytes(picture.png(i, *size, texture=True))
            self.sizes[i] = size
        # A real 24-megapixel JPEG exercises the decode/resize path users hit.
        large = self.root / "large.png"
        large.write_bytes(picture.png(20, 6000, 4000, texture=True))
        subprocess.run(["convert", str(large), "-quality", "92", str(self.library / "large.jpg")],
                       check=True, timeout=60, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.sizes[20] = (6000, 4000)
        # Only the existing download transport is replaced; theme get, saving,
        # decoding, layout and the real kitten renderer all run unchanged.
        curl = self.root / "bin/curl"
        curl.write_text('#!/bin/sh\nset -eu\nout=\nwhile [ "$#" -gt 0 ]; do\n'
                        '  if [ "$1" = -o ]; then shift; out=$1; fi\n  shift\ndone\n'
                        '[ -n "$out" ]\ncp "$THEME_WALLPAPER_DIR/large.jpg" "$out"\n')
        curl.chmod(0o500)
        stub = self.root / "bin/wallpaper"
        stub.write_text('#!/bin/sh\n[ "$#" -eq 1 ] && [ "$1" = get ] || exit 64\n'
                        f'printf "%s\\n" "{self.library}/fixture-000.png"\n')
        stub.chmod(0o500)
        self.env = {
            "PATH": f"{self.root / 'bin'}:{kitty_bin}:/usr/bin:/bin",
            "HOME": str(self.root), "LANG": "C", "LC_ALL": "C", "TZ": "UTC",
            "THEME_WALLPAPER_DIR": str(self.library), "THEME_CACHE_DIR": str(self.root / "cache"),
            "CONFIG_DIR": str(self.root / "config"),
            "KITTY_CONFIG_DIRECTORY": str(self.root / "kitty-config"),
            "TMPDIR": str(self.root / "tmp"), "THEME_NO_UPDATE_CHECK": "1",
            "THEME_NO_APPLY": "1", "THEME_FORMATS": "png", "THEME_EXCLUDE_FORMATS": "",
            "THEME_CONTRAST": "7", "THEME_OPACITY": "0.9", "THEME": str(theme),
        }
        if "DISPLAY" in os.environ:
            self.env["DISPLAY"] = os.environ["DISPLAY"]
        if "XAUTHORITY" in os.environ:
            self.env["XAUTHORITY"] = os.environ["XAUTHORITY"]

    def remove(self):
        shutil.rmtree(self.root, ignore_errors=True)
        shutil.rmtree(self.socket_dir, ignore_errors=True)


class Session:
    """A running Kitty with `theme browse` inside it, driven over its own
    private socket and read back with `kitten @ get-text`."""

    def __init__(self, args, fixture, output, command=None):
        self.kitty, self.output, self.fixture = Path(args.kitty), output, fixture
        self.kitten = self.kitty.parent / "kitten"
        if not self.kitten.is_file():
            raise Failure(f"no kitten next to {self.kitty}")
        self.socket = fixture.socket_dir / "kitty.sock"
        self.log = (output / "kitty.log").open("wb")
        argv = [str(self.kitty), "--config", "NONE",
                "-o", "allow_remote_control=socket-only", "-o", "font_size=11",
                "-o", "background=#000000", "-o", "foreground=#ffffff",
                "-o", "placement_strategy=top-left", "-o", "window_padding_width=0",
                "-o", "hide_window_decorations=yes", "-o", "confirm_os_window_close=0",
                # macOS keeps the app alive with no windows by default, which
                # would make "the browser quit" unobservable from outside.
                "-o", "macos_quit_when_last_window_closed=yes",
                "--listen-on", f"unix:{self.socket}", "--title", "theme-e2e"]
        # Ask for the whole screen by size, not by state: plain numbers are
        # pixels (a unit would be ignored, leaving kitty's own 640x400, too
        # small for a sheet), and a bare Xvfb has no window manager to honour
        # a fullscreen request at all. At exactly the screen's size the
        # capture and the cell grid share one origin on both platforms.
        argv += ["-o", f"initial_window_width={WINDOW[0]}",
                 "-o", f"initial_window_height={WINDOW[1]}"]
        if command is None:
            argv += ["--", str(args.theme), "browse", "--all", "--page-size", str(PAGE_SIZE)]
        else:
            # Hold only this test window after a one-shot command, preserving
            # its exit status and visible frame for the controller.
            argv += ["--", "/bin/sh", "-c",
                     '\"$@\"; result=$?; printf "\\nTHEME_E2E_DONE=%s\\n" "$result"; read answer',
                     "theme-e2e", str(args.theme), *command]
        self.argv = argv
        self.process = subprocess.Popen(argv, env=fixture.env, stdin=subprocess.DEVNULL,
                                        stdout=self.log, stderr=self.log)
        self.wait_for_socket()
        self.geometry = self.window()

    def remote(self, *arguments, stdin=None):
        return subprocess.run([str(self.kitten), "@", "--to", f"unix:{self.socket}", *arguments],
                              input=stdin, env=self.fixture.env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, timeout=30)

    def wait_for_socket(self):
        deadline = time.monotonic() + SOCKET_WAIT
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise Failure(f"kitty exited {self.process.returncode} before answering; "
                              f"log: {self.read_log()[-1500:]!r}")
            if self.remote("ls").returncode == 0:
                return
            time.sleep(POLL)
        raise Failure(f"kitty never answered its socket in {SOCKET_WAIT:.0f}s; "
                      f"log: {self.read_log()[-1500:]!r}")

    def read_log(self):
        self.log.flush()
        return (self.output / "kitty.log").read_text(errors="replace")

    def window(self):
        """Columns, lines and the platform window id, from kitty itself."""
        listed = json.loads(self.remote("ls").stdout)
        os_window = listed[0]
        window = os_window["tabs"][0]["windows"][0]
        return {"columns": window["columns"], "lines": window["lines"],
                "platform_window_id": os_window.get("platform_window_id"),
                "cards": min(max(window["columns"] // 32, 1), 3)}

    def text(self, extent="screen"):
        """What the terminal says. Assertions poll the visible screen; the
        scrollback is read only where the output is taller than the screen."""
        result = self.remote("get-text", "--match", "all", "--extent", extent)
        if result.returncode != 0:
            raise Failure(f"get-text failed: {result.stderr[-500:]!r}")
        return result.stdout.decode("utf-8", "replace")

    def ended(self):
        """The browser is gone: kitty exited, or it has no windows left."""
        if self.process.poll() is not None:
            return True
        listed = self.remote("ls")
        if listed.returncode != 0:
            return True
        try:
            return not json.loads(listed.stdout)
        except ValueError:
            return False

    def send(self, keys):
        result = self.remote("send-text", "--match", "all", "--stdin", stdin=keys)
        if result.returncode != 0:
            raise Failure(f"send-text failed: {result.stderr[-500:]!r}")

    def wait_until(self, description, predicate):
        """Poll the screen until it satisfies `predicate`. A timeout dumps the
        screen it gave up on — a failing terminal test is unreadable without
        the screen."""
        deadline = time.monotonic() + ASSERT_WAIT
        screen = ""
        while time.monotonic() < deadline:
            screen = self.text()
            if predicate(screen):
                return screen
            time.sleep(POLL)
        self.dump("timeout", screen)
        raise Failure(f"{description} never happened; screen was:\n{screen}")

    def dump(self, name, screen=None):
        path = self.output / f"screen-{name}.txt"
        path.write_text(screen if screen is not None else self.text())
        return path

    def screenshot(self, name):
        """The window as pixels. Linux captures the X root, which fullscreen
        makes the window; macOS captures the window by its own id, without the
        shadow, so both captures start at the cell grid's origin."""
        path = self.output / f"{name}.png"
        if platform.system() == "Linux":
            tool = ["import", "-window", "root", f"png24:{path}"]
        else:
            window_id = self.geometry["platform_window_id"]
            if not window_id:
                raise Failure("kitty reported no platform window id to capture")
            tool = ["screencapture", "-x", "-o", "-l", str(window_id), str(path)]
        result = subprocess.run(tool, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
        if result.returncode != 0 or not path.is_file():
            raise Failure(f"{tool[0]} failed: {result.stderr[-500:]!r}")
        return path

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=10)
        self.log.close()


def settled(screen):
    """Whether the browser has finished answering the last key: its prompt
    is printed after everything else, so the prompt being the final
    non-empty row is what says the sheet, preview or help above it is whole.

    A page counter is the FIRST line of a sheet, so waiting on the counter
    alone reads a screen that is still being drawn. On a hosted runner that
    put a page-2 header on rows 47-48 of a 50-line screen, the band was
    computed at rows 49-56, the sheet then finished and scrolled the
    pictures up to rows 24-47, and every retry measured the stale band
    (#87). Every wait below pairs its own expectation with this.
    """
    rows = [row for row in screen.splitlines() if row.strip()]
    return bool(rows) and rows[-1].startswith(PROMPT)


def query_row(screen):
    """The row of the most recent sheet's query line: the header the seven
    reserved thumbnail rows follow. Output is appended, so the last one on
    screen belongs to the sheet just drawn."""
    rows = screen.splitlines()
    found = [i for i, row in enumerate(rows) if row.startswith("Query:")]
    return found[-1] if found else None


def sheet_cards(screen, top):
    """The cards of the title row under a sheet's thumbnails, in the order
    the browser laid them out. Which picture the queue starts on is the
    library's business — the search order differs between filesystems — so
    every selection assertion is derived from here, never from a name."""
    rows = screen.splitlines()
    row = rows[top + THUMBNAIL_ROWS] if top + THUMBNAIL_ROWS < len(rows) else ""
    return re.findall(r"(\d+)\s+(fixture-\d{3}\.png)", row)


def drawn_images(session, name, screen, top, bottom, expected, palette=None):
    """Retry actual pixel matching until GL paints; keep every final result."""
    attempts, deadline = 0, time.monotonic() + ASSERT_WAIT
    while True:
        attempts += 1
        shot = session.screenshot(name)
        capture = shot.read_bytes()
        size = struct.unpack(">II", capture[16:24])
        cell = (size[0] // session.geometry["columns"], size[1] // session.geometry["lines"])
        boxes = picture.rectangles(screen, top, bottom, cell)
        image = decode_png(capture, max_rows=max(bottom, palette[0] + 1 if palette else 0) * cell[1])
        matches = [picture.match(image, box, number, session.fixture.sizes[number])
                   for box, number in zip(boxes, expected)]
        palette_ok = palette is None or picture.swatches(image, *palette, cell)
        drawn = bool(expected) and len(boxes) == len(expected) and all(item["ok"] for item in matches) and palette_ok
        if drawn or time.monotonic() >= deadline:
            return {"file": shot.name, "cell": cell, "captures": attempts,
                    "boxes": boxes, "expected": expected, "matches": matches,
                    "palette_drawn": palette_ok, "drawn": drawn}
        time.sleep(POLL)


def selected_capture(session, name, screen, expected):
    rows = screen.splitlines()
    title = max(i for i, row in enumerate(rows) if re.fullmatch(r"\d+ fixture-\d{3}\.png", row))
    top = title
    while top and PLACEHOLDER in rows[top - 1]:
        top -= 1
    session.dump(name, screen)
    palette_row = max(i for i, row in enumerate(rows) if row.strip() == "COLORSCHEME") + 1
    palette = palette_for(session, expected[1])
    capture = drawn_images(session, name, screen, top, title, [int(expected[1][8:11])],
                           (palette_row, 0, palette))
    capture["minimal"] = clean_preview("\n".join(rows[top:]), " ".join(expected))
    return capture


def palette_for(session, filename):
    result = subprocess.run([session.fixture.env["THEME"], "preview", str(session.fixture.library / filename)],
                            env=session.fixture.env, capture_output=True, check=True, timeout=30)
    return [tuple(map(int, rgb)) for rgb in
            re.findall(rb"\x1b\[48;2;(\d+);(\d+);(\d+)m", result.stdout)]


def clean_preview(screen, title, saved=False):
    rows = []
    for row in screen.splitlines():
        if PLACEHOLDER in row and all(c == PLACEHOLDER or c.isspace() or
                                     picture.unicodedata.category(c) in ("Mn", "Me") for c in row):
            continue
        if row.strip() and row.strip() not in (PROMPT, "THEME_E2E_DONE=0"):
            rows.append(" ".join(row.split()))
    expected = [title, "COLORSCHEME"]
    if saved:
        expected.insert(0, "theme: saved download.jpg (6000x4000)")
    return rows == expected


def last_page(screen):
    """The page counter of the most recent sheet: output is appended, so the
    last counter on screen is the one this key produced."""
    found = re.findall(r"page (\d+)/(\d+)", screen)
    return f"{found[-1][0]}/{found[-1][1]}" if found else None


def last_title(screen):
    """The title line of the most recent preview. Only a preview prints a
    title alone on its line; the sheet prints `cards` of them side by side."""
    found = re.findall(r"(?m)^(\d+) (fixture-\d{3}\.png)$", screen)
    return found[-1] if found else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kitty", required=True, help="the pinned kitty binary")
    parser.add_argument("--theme", required=True, help="the theme binary under test")
    parser.add_argument("--output", required=True, type=Path, help="empty directory for evidence")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        print("FAIL --output must be empty; existing evidence is never overwritten")
        return 1
    theme = Path(args.theme).resolve(strict=True)
    kitty = Path(args.kitty).resolve(strict=True)
    version = subprocess.run([str(kitty), "--version"], stdout=subprocess.PIPE,
                             timeout=60).stdout.decode(errors="replace").strip()
    summary = {"kitty": version, "platform": platform.platform(), "theme": str(theme),
               "sha256": hashlib.sha256(theme.read_bytes()).hexdigest(), "images": IMAGES, "page_size": PAGE_SIZE, "assertions": []}
    passed = True

    def check(name, ok, detail=""):
        nonlocal passed
        passed = passed and bool(ok)
        summary["assertions"].append({"name": name, "ok": bool(ok), "detail": detail})
        print(f"{'OK  ' if ok else 'FAIL'} {name}{': ' + detail if detail else ''}", flush=True)

    fixture = Fixture(theme, kitty.parent, output)
    session = None
    try:
        session = Session(args, fixture, output)
        summary["geometry"] = session.geometry
        columns, lines = session.geometry["columns"], session.geometry["lines"]
        check("kitty runs the browser", True, f"{version}, {columns}x{lines} cells")
        # Two cards per row at the least: a lone card would make the sheet's
        # own title row look like a preview's, and the title assertions below
        # would stop meaning anything.
        check("window is wide enough for the sheet", session.geometry["cards"] >= 2,
              f"{session.geometry['cards']} cards per row")

        # A sheet taller than the screen scrolls its own header away, and
        # then neither the page counter nor the thumbnail rows are where the
        # screen says they are. Prove it fits before trusting either.
        card_rows = -(-PAGE_SIZE // session.geometry["cards"])
        needed = 3 + card_rows * (THUMBNAIL_ROWS + 3) + 2
        check("the whole sheet fits the screen", lines >= needed,
              f"{lines} lines, {needed} needed for {card_rows} card rows")
        # Settled first, always: a half-drawn sheet must never answer an
        # assertion, and the band is located from the screen that answers.
        screen = session.wait_until(f"the first sheet ({PAGES} pages)",
                                    lambda s: settled(s) and last_page(s) == f"1/{PAGES}")
        check("first sheet is page 1", last_page(screen) == f"1/{PAGES}", f"page {last_page(screen)}")
        session.dump("sheet", screen)

        # The thumbnails, as pixels. The sheet prints the page header, the
        # query line, then this card row's THUMBNAIL_ROWS reserved rows.
        query = query_row(screen)
        check("sheet header is on screen", query is not None, f"Query: at row {query}")
        cards = []
        if query is not None:
            top = query + 1
            # Two halves, and the evidence says which one broke: the sheet
            # asking for images (placeholder cells in the screen text), and
            # the terminal having drawn them (colour in those rows' pixels).
            asked = sum(1 for row in screen.splitlines()[top:top + THUMBNAIL_ROWS]
                        if PLACEHOLDER in row)
            check("every thumbnail row asks the terminal for an image",
                  asked == THUMBNAIL_ROWS, f"{asked}/{THUMBNAIL_ROWS} rows with image cells")
            cards = sheet_cards(screen, top)
            check("the sheet names its cards", len(cards) >= 2, repr(cards))
            capture = drawn_images(session, "sheet", screen, top, top + THUMBNAIL_ROWS,
                                   [int(card[1][8:11]) for card in cards])
            summary["capture"] = capture
            check("sheet paints the named pictures at their source aspect ratios", capture["drawn"],
                  json.dumps(capture["matches"]))

        # The keys a person presses, in the order a person presses them. Which
        # picture the queue starts on is the library's business, not this
        # test's — the search order differs between filesystems — so the
        # selection is asserted by movement, never by an absolute name.
        if len(cards) < 2:
            raise Failure("the sheet's title row gave no two cards to steer by")
        first, second = cards[0], cards[1]
        session.send(b"\x1b[C")
        try:
            screen = session.wait_until("Right previews the sheet's first card",
                               lambda s: settled(s) and last_title(s) == first)
            check("Right previews the sheet's first card", True, f"title {' '.join(first)}")
            capture = selected_capture(session, "selected-first", screen, first)
            summary["selected_capture"] = capture
            check("selected preview paints the first picture", capture["drawn"], json.dumps(capture["matches"]))
            # Inspect only the latest frame, not earlier browser sheets.
            check("selected preview is picture, name and colorscheme", capture["minimal"])
        except Failure as error:
            check("Right previews the sheet's first card", False, str(error).splitlines()[0])

        for keys, name, want in [(b"\x1b[B", "Down", f"2/{PAGES}"),
                                 (b"\x1b[6~", "PageDown", f"3/{PAGES}"),
                                 (b"\x1b[5~", "PageUp", f"2/{PAGES}"),
                                 (b" ", "Space", f"3/{PAGES}"),
                                 (b"\x1b[H", "Home", f"1/{PAGES}"),
                                 (b"\x1b[F", "End", f"3/{PAGES}")]:
            session.send(keys)
            try:
                screen = session.wait_until(f"{name} shows page {want}",
                                            lambda s: settled(s) and last_page(s) == want)
                check(f"{name} shows page {want}", True, f"page {last_page(screen)}")
            except Failure as error:
                check(f"{name} shows page {want}", False, str(error).splitlines()[0])
            if name == "Down":
                # `screen` is settled, so this header is the finished sheet's.
                session.dump("page-change", screen)
                turned = query_row(screen)
                if turned is None:
                    raise Failure("the turned page printed no header to measure from")
                top = turned + 1
                next_cards = sheet_cards(screen, top)
                capture = drawn_images(session, "page-change", screen, top, top + THUMBNAIL_ROWS,
                                       [int(card[1][8:11]) for card in next_cards])
                summary["page_change_capture"] = capture
                check("page change paints the newly named pictures", capture["drawn"],
                      json.dumps(capture["matches"]))

        for keys, name, want in [(b"n\n", "n steps to the second card", second),
                                 (b"p\n", "p steps back to the first", first)]:
            session.send(keys)
            try:
                screen = session.wait_until(f"{name} ({want})",
                                            lambda s: settled(s) and last_title(s) == want)
                check(name, True, f"title {' '.join(last_title(screen))}")
                capture = selected_capture(session, name.split()[0], screen, want)
                summary[name.split()[0] + "_capture"] = capture
                check(name + " paints the correct picture", capture["drawn"], json.dumps(capture["matches"]))
                check(name + " has only name and colorscheme text", capture["minimal"])
            except Failure as error:
                check(name, False, str(error).splitlines()[0])

        # The usage text is taller than the screen, so its tail is what the
        # screen can hold; the scrollback proves the key map is in it.
        session.send(b"?\n")
        try:
            session.wait_until("? prints the usage text",
                               lambda s: settled(s) and "THEME_NO_APPLY" in s)
            printed = session.text(extent="all")
            check("? prints the usage text naming the keys",
                  "theme browse [terms...]" in printed
                  and "Keys (pressed on an empty line" in printed,
                  "usage text with its key map")
            session.dump("help")
        except Failure as error:
            check("? prints the usage text naming the keys", False, str(error).splitlines()[0])

        session.send(b"q\n")
        deadline = time.monotonic() + ASSERT_WAIT
        while time.monotonic() < deadline and not session.ended():
            time.sleep(POLL)
        check("q ends the session", session.ended(),
              f"kitty exited {session.process.returncode}" if session.process.poll() is not None
              else "no windows left" if session.ended() else "the browser is still running")
        session.close()
        session = None
        for command in (["preview", "large.jpg"], ["get", "https://img.invalid/download.jpg"]):
            name = command[0]
            child = output / name
            child.mkdir()
            session = Session(args, fixture, child, command)
            screen = session.wait_until(name + " completes", lambda s: "THEME_E2E_DONE=" in s)
            session.dump(name, screen)
            check(name + " exits successfully", "THEME_E2E_DONE=0" in screen)
            stem = "large" if name == "preview" else "download"
            check(name + " has minimal output", clean_preview(screen, "TITLE " + stem, saved=name == "get"))
            palette_row = max(i for i, row in enumerate(screen.splitlines()) if row.strip() == "COLORSCHEME") + 1
            capture = drawn_images(session, name, screen, 0, session.geometry["lines"], [20],
                                   (palette_row, 2, palette_for(session, stem + ".jpg")))
            summary[name + "_capture"] = capture
            check(name + " paints the 6000x4000 JPEG", capture["drawn"], json.dumps(capture["matches"]))
            if name == "get":
                check("get saved the downloaded bytes", (fixture.library / "download.jpg").read_bytes()
                      == (fixture.library / "large.jpg").read_bytes())
            check(name + " applies no palette", not (fixture.root / "cache/wal").exists())
            session.close()
            session = None
    except (Failure, OSError, ValueError, subprocess.SubprocessError) as error:
        check("driver completed", False, str(error).splitlines()[0])
        summary["failure"] = str(error)
    finally:
        if session is not None:
            summary["kitty_log"] = session.read_log()[-4000:]
            summary["argv"] = session.argv
            session.close()
        (output / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
        fixture.remove()
    print(f"kitty end-to-end: {'PASS' if passed else 'FAIL'}; evidence in {output}")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
