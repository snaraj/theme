#!/usr/bin/env python3
"""Graphics transport, geometry and browser reuse through a real PTY.

The helper is deterministic; this proves protocol bytes and navigation,
not drawn pixels. kitty_e2e.py owns the real-renderer check.
"""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

spec = importlib.util.spec_from_file_location("browser_test", Path(__file__).with_name("browser_cli_test.py"))
browser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser)


def main():
    binary = Path(sys.argv[1]).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="theme-preview-") as directory:
        root = Path(directory)
        for name in ("bin", "library", "config", "tmp"):
            (root / name).mkdir()
        picture = root / "library/blue.png"
        browser.png(picture, (32, 64, 128))
        helper = root / "bin/kitten"
        helper.write_text(f"#!{sys.executable}\n" + '''import json, os, sys
with open(os.environ["PREVIEW_CALLS"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
if os.environ.get("PREVIEW_FAILURE"):
    sys.exit(1)
if os.environ.get("PREVIEW_INVALID"):
    sys.stdout.write("unusable image output")
    sys.exit(0)
sys.stdout.write("\\r\\x1b_Ga=T,c=1,r=1,m=1;YWJj\\x1b\\\\"
                 "\\x1b_Gm=0;ZA==\\x1b\\\\"
                 "\\x1b[38:2:1:2:3m\\U0010eeee\\u0305\\u0305\\n")
''')
        helper.chmod(0o700)
        calls = root / "calls.jsonl"
        env = os.environ | {"PATH": f"{root / 'bin'}:/usr/bin:/bin", "HOME": str(root), "KITTY_LISTEN_ON": "",
            "CONFIG_DIR": str(root / "config"),
            "KITTY_CONFIG_DIRECTORY": str(root / "config"), "THEME_CACHE_DIR": str(root / "cache"),
            "THEME_WALLPAPER_DIR": str(root / "library"), "THEME_NO_APPLY": "1",
            "THEME_NO_UPDATE_CHECK": "1", "THEME_OPACITY": "0.8", "THEME_CONTRAST": "4.5",
            "THEME_FORMATS": "png", "THEME_EXCLUDE_FORMATS": "", "TMPDIR": str(root / "tmp"),
            "KITTY_WINDOW_ID": "1", "TERM": "xterm-kitty", "PREVIEW_CALLS": str(calls)}
        for args in (["preview", str(picture)], ["list", "-v"], ["browse", "--all"]):
            result = subprocess.run([str(binary), *args], env=env, capture_output=True, timeout=15)
            browser.require(result.returncode == 0, result.stderr)
            browser.require(b"\x1b_G" not in result.stdout and "\U0010eeee".encode() not in result.stdout,
                            f"redirected {args} leaked graphics")
            browser.require(b"image preview unavailable" not in result.stdout + result.stderr,
                            f"redirected {args} printed an image notice")
        browser.require(not calls.exists(), "a pipe started the graphics helper")

        def count():
            return len(calls.read_text().splitlines())

        with browser.Terminal(binary, env, ["browse", "--all"], columns=80) as term:
            term.expect("browse> ")
            browser.require(count() == 1, "first sheet must render one image")
            term.press(b"list\n", "browse> ")
            browser.require(count() == 1, "revisiting the sheet decoded the image again")
            term.press(b"select 1\n", "browse> ")
            browser.require(count() == 2, "selection needs its larger thumbnail")
            term.press(b"select 1\n", "browse> ")
            browser.require(count() == 2, "revisiting the selection decoded the image again")
            browser.png(picture, (64, 128, 32))
            term.press(b"select 1\n", "browse> ")
            browser.require(count() == 3, "image edit left a stale cached thumbnail")
            import fcntl
            import struct
            import termios
            fcntl.ioctl(term.master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 80, 800, 1200))
            term.press(b"select 1\n", "browse> ")
            browser.require(count() == 4, "pixel geometry change left a stale thumbnail")
            records = [json.loads(line) for line in calls.read_text().splitlines()]
            browser.require(all("--transfer-mode=stream" in record for record in records), "file transfer returned")
            browser.require("80,40,800,1200" in records[-1], "actual pixel dimensions were ignored")
            raw = bytes(term.output)
            browser.require(raw.count(b"\x1b_Ga=T") == raw.count(b"\x1b_Gm=0"), "a streamed packet was dropped")
            browser.require(b"\x1b[38:2:1:2:3m" in raw, "image ID colour was lost")
            browser.require(b"image preview unavailable" not in raw, "successful rendering printed a failure")
            term.leave()

        for overrides, columns, reason in [
            ({"KITTY_WINDOW_ID": "", "TERM": "xterm-256color"}, 80, "use Kitty to view pictures"),
            ({"PATH": str(root / "empty-bin")}, 80, "kitten helper was not found on PATH"),
            ({"PREVIEW_FAILURE": "1"}, 80, "image renderer failed for this file"),
            ({"PREVIEW_INVALID": "1"}, 80, "image renderer failed for this file"),
            ({}, 7, "window is too narrow"),
        ]:
            for args in (["preview", str(picture)], ["browse", "--all"]):
                with browser.Terminal(binary, env | overrides, args, columns=columns) as term:
                    if args[0] == "browse":
                        prompt = "browse> " if columns >= 8 else "browse…"
                        term.expect(prompt)
                        term.press(b"select 1\n", prompt)
                        term.press(b"select 1\n", prompt)
                        term.leave()
                    else:
                        term.leave(b"")
                    text = term.screen()
                    compact = "".join(text.split())
                    browser.require("".join(reason.split()) in compact, f"missing image notice: {text!r}")
                    browser.require(compact.count("imagepreviewunavailable") == 1, "repeated image notice")
                    browser.require("COLORSCHEME" in compact, "missing palette fallback")
                    browser.require(str(root) not in text, "fallback printed full filesystem paths")
                    for unwanted in ["Sampled readability", "Measured image", "editor.go", "go test",
                                     "example diagnostic", "selected text", "Preview only"]:
                        browser.require("".join(unwanted.split()) not in compact, f"demo output returned: {unwanted}")
                    raw = bytes(term.output)
                    browser.require(b"\x1b_G" not in raw and "\U0010eeee".encode() not in raw,
                                    "an unavailable preview leaked graphics")
        browser.require(not (root / "cache").exists(), "dry-run navigation wrote a cache")
        # Applying into disposable config/cache directories with NO helpers on
        # PATH cannot change a desktop or contact a terminal. The Kitty marker
        # also keeps OSC away from the process's controlling tty.
        empty = root / "empty-bin"
        empty.mkdir()
        config = root / "config/kitty.conf"
        apply_env = env | {"PATH": str(empty), "THEME_CONTRAST": "7"}
        apply_env.pop("THEME_NO_APPLY", None)
        apply_env.pop("THEME_OPACITY", None)
        # Force a dark region too, retaining a known valid PNG fixture.
        import struct
        import zlib
        def chunk(kind, data):
            return struct.pack("!I", len(data)) + kind + data + struct.pack("!I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        picture.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack("!2I5B", 2, 1, 8, 2, 0, 0, 0))
                            + chunk(b"IDAT", zlib.compress(b"\0\0\0\0\xff\xff\xff")) + chunk(b"IEND", b""))
        for opacity in ("0.1", "1.0"):
            config.write_text(f"background_opacity {opacity}\nmap ctrl+x no_op\n")
            original = config.read_bytes()
            result = subprocess.run([str(binary), "set", str(picture)], env=apply_env, capture_output=True, timeout=15)
            browser.require(result.returncode == 1 and b"Kitty colors were not applied" in result.stderr,
                            "a missing Kitty socket silently succeeded: " + repr(result.stderr))
            browser.require((result.stdout + result.stderr).count(b"text may be hard to read") == (1 if opacity == "0.1" else 0),
                            "apply omitted, repeated, or unnecessarily emitted its opacity warning")
            browser.require(config.read_bytes() == original, "apply changed opacity or key bindings")
            palette = (root / "cache/colors-kitty.conf").read_text()
            browser.require("background_opacity" not in palette, "palette changes opacity through its include")
            preview = subprocess.run([str(binary), "preview", str(picture)], env=apply_env, capture_output=True, timeout=15)
            browser.require(preview.returncode == 0 and b"text may be hard to read" not in preview.stdout + preview.stderr,
                            "preview printed apply advice")
        wallpaper = root / "bin/wallpaper"
        wallpaper.write_text(f'#!/bin/sh\nprintf "%s\\n" "$2" > "{root}/desktop-called"\n')
        wallpaper.chmod(0o700)
        broken = root / "library/broken.png"
        broken.write_bytes(b"invalid image")
        blocked = root / "blocked-cache"
        blocked.write_text("not a directory")
        for image, overrides in [(broken, {}), (picture, {"THEME_CACHE_DIR": str(blocked)})]:
            result = subprocess.run([str(binary), "set", str(image)],
                env=apply_env | {"PATH": env["PATH"]} | overrides, capture_output=True, timeout=15)
            browser.require(result.returncode == 1 and not (root / "desktop-called").exists(),
                            "failed palette preparation changed the desktop")
    print("preview CLI: PASS (pipes, notices, stream packets, geometry, reuse, invalidation, terminal restore)")


if __name__ == "__main__":
    main()
