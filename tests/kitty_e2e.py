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

IMAGES = 14
PAGE_SIZE = 6  # 14 images over 6 per page: three pages, the last one short
PAGES = -(-IMAGES // PAGE_SIZE)
WINDOW = (1400, 900)
THUMBNAIL_ROWS = 7  # what the sheet reserves per card, before title and swatches
PLACEHOLDER = "\U0010eeee"  # the cell kitty fills with a transmitted image
SOCKET_WAIT = 30.0
ASSERT_WAIT = 10.0
POLL = 0.2


class Failure(Exception):
    """An assertion that did not hold, with the screen behind it."""


def png(number, width, height):
    """One deterministic picture: gradients in all three channels, so a
    thumbnail of it cannot be mistaken for a flat background."""
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    pixels = bytearray()
    for y in range(height):
        pixels.append(0)
        for x in range(width):
            pixels.extend(((x * 3 + number * 17) % 256, (y * 5 + number * 11) % 256,
                           (128 + x + y + number * 7) % 256))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(pixels), 6)) + chunk(b"IEND", b""))


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


def band_colours(image, top, bottom):
    """Distinct colours and the non-background fraction in a horizontal band.
    The background is whatever colour the decoded part of the screen has most
    of — a terminal is mostly background, drawn or not."""
    width, channels, pixels = image["width"], image["channels"], image["pixels"]
    counts = {}
    for offset in range(0, len(pixels), channels):
        colour = pixels[offset:offset + 3]
        counts[colour] = counts.get(colour, 0) + 1
    background = max(counts, key=counts.get)
    seen, foreground, total = set(), 0, 0
    for y in range(top, min(bottom, image["rows"])):
        row = y * width * channels
        for x in range(width):
            colour = pixels[row + x * channels:row + x * channels + 3]
            seen.add(colour)
            total += 1
            if max(abs(a - b) for a, b in zip(colour, background)) > 12:
                foreground += 1
    return {"colours": len(seen), "pixels": total,
            "foreground": round(foreground / total, 4) if total else 0.0,
            "background": "#%02x%02x%02x" % tuple(background)}


class Fixture:
    """A private library, cache, config and PATH: this test reads and writes
    nothing of the machine it runs on."""

    def __init__(self, theme, kitty_bin):
        self.root = Path(tempfile.mkdtemp(prefix="kitty-e2e-"))
        self.library = self.root / "library"
        for child in ("library", "cache", "config/kitty", "kitty-config", "tmp", "bin"):
            (self.root / child).mkdir(mode=0o700, parents=True)
        self.sizes = {}
        for i in range(IMAGES):
            # A distinct size per picture: the preview's dimensions line then
            # names which picture the selection is on.
            size = (240 + i * 8, 160 + i * 4)
            (self.library / f"fixture-{i:03d}.png").write_bytes(png(i, *size))
            self.sizes[i] = size
        stub = self.root / "bin/wallpaper"
        stub.write_text('#!/bin/sh\n[ "$#" -eq 1 ] && [ "$1" = get ] || exit 64\n'
                        f'printf "%s\\n" "{self.library}/fixture-000.png"\n')
        stub.chmod(0o500)
        self.env = {
            "PATH": f"{kitty_bin}:{self.root / 'bin'}:/usr/bin:/bin",
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


class Session:
    """A running Kitty with `theme browse` inside it, driven over its own
    private socket and read back with `kitten @ get-text`."""

    def __init__(self, args, fixture, output):
        self.kitty, self.output, self.fixture = Path(args.kitty), output, fixture
        self.kitten = self.kitty.parent / "kitten"
        if not self.kitten.is_file():
            raise Failure(f"no kitten next to {self.kitty}")
        self.socket = fixture.root / "kitty.sock"
        self.log = (output / "kitty.log").open("wb")
        argv = [str(self.kitty), "--config", "NONE",
                "-o", "allow_remote_control=yes", "-o", "font_size=11",
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
        argv += ["--", str(args.theme), "browse", "--all", "--page-size", str(PAGE_SIZE)]
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


def drawn_band(session, name, top, columns, lines):
    """Capture the window until the thumbnail band holds pictures, or the
    wait runs out. Transmitting an image and drawing it are not the same
    instant: a hosted runner on software GL had the sheet's text on screen
    with the pictures not yet painted, so one capture can race the paint. A
    sheet that never draws them still fails at the deadline, which is the
    whole point of the assertion, and the last capture is the one kept.
    """
    attempts, deadline = 0, time.monotonic() + ASSERT_WAIT
    while True:
        attempts += 1
        shot = session.screenshot(name)
        capture = shot.read_bytes()
        # The cell grid comes from the capture's own size over the grid kitty
        # reports, so a Retina capture needs no special case; only the rows
        # this band needs are decoded.
        size = struct.unpack(">II", capture[16:24])
        cell = {"width": size[0] // columns, "height": size[1] // lines}
        image = decode_png(capture, max_rows=(top + THUMBNAIL_ROWS) * cell["height"] + 1)
        band = band_colours(image, top * cell["height"], (top + THUMBNAIL_ROWS) * cell["height"])
        drawn = band["colours"] >= 64 and band["foreground"] >= 0.05
        if drawn or time.monotonic() >= deadline:
            return {"file": shot.name, "width": image["width"], "height": image["height"],
                    "decoded_rows": image["rows"], "cell": cell, "captures": attempts,
                    "band_rows": [top, top + THUMBNAIL_ROWS], "thumbnails": band, "drawn": drawn}
        time.sleep(POLL)


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
               "images": IMAGES, "page_size": PAGE_SIZE, "assertions": []}
    passed = True

    def check(name, ok, detail=""):
        nonlocal passed
        passed = passed and bool(ok)
        summary["assertions"].append({"name": name, "ok": bool(ok), "detail": detail})
        print(f"{'OK  ' if ok else 'FAIL'} {name}{': ' + detail if detail else ''}", flush=True)

    fixture = Fixture(theme, kitty.parent)
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
        # Wait for the prompt: it is printed last, so a half-drawn sheet can
        # never answer an assertion.
        screen = session.wait_until(f"the first sheet ({PAGES} pages)",
                                    lambda s: "browse>" in s and last_page(s) == f"1/{PAGES}")
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
            capture = drawn_band(session, "sheet", top, columns, lines)
            summary["capture"] = capture
            band = capture["thumbnails"]
            check("thumbnail rows are pictures, not empty cells", capture["drawn"],
                  f"{band['colours']} distinct colours, {band['foreground'] * 100:.1f}% "
                  f"non-background (background {band['background']}), cell "
                  f"{capture['cell']['width']}x{capture['cell']['height']}px, "
                  f"{capture['captures']} capture(s)")
            cards = sheet_cards(screen, top)
            check("the sheet names its cards", len(cards) >= 2,
                  " · ".join(f"{n} {name}" for n, name in cards[:3]) or "no title row found")

        # The keys a person presses, in the order a person presses them. Which
        # picture the queue starts on is the library's business, not this
        # test's — the search order differs between filesystems — so the
        # selection is asserted by movement, never by an absolute name.
        if len(cards) < 2:
            raise Failure("the sheet's title row gave no two cards to steer by")
        first, second = cards[0], cards[1]
        session.send(b"\x1b[C")
        try:
            session.wait_until("Right previews the sheet's first card",
                               lambda s: last_title(s) == first)
            check("Right previews the sheet's first card", True, f"title {' '.join(first)}")
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
                                            lambda s: last_page(s) == want)
                check(f"{name} shows page {want}", True, f"page {last_page(screen)}")
            except Failure as error:
                check(f"{name} shows page {want}", False, str(error).splitlines()[0])
            if name == "Down":
                session.dump("page-change", screen)
                turned = query_row(screen)
                if turned is None:
                    raise Failure("the turned page printed no header to measure from")
                capture = drawn_band(session, "page-change", turned + 1, columns, lines)
                summary["page_change_capture"] = capture
                band = capture["thumbnails"]
                check("the page that was turned to is pictures too", capture["drawn"],
                      f"{band['colours']} distinct colours, {band['foreground'] * 100:.1f}% "
                      f"non-background, {capture['captures']} capture(s)")

        for keys, name, want in [(b"n\n", "n steps to the second card", second),
                                 (b"p\n", "p steps back to the first", first)]:
            session.send(keys)
            try:
                screen = session.wait_until(f"{name} ({want})", lambda s: last_title(s) == want)
                check(name, True, f"title {' '.join(last_title(screen))}")
            except Failure as error:
                check(name, False, str(error).splitlines()[0])

        # The usage text is taller than the screen, so its tail is what the
        # screen can hold; the scrollback proves the key map is in it.
        session.send(b"?\n")
        try:
            session.wait_until("? prints the usage text", lambda s: "THEME_NO_APPLY" in s)
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
    except Failure as error:
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
