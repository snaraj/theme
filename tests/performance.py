#!/usr/bin/env python3
"""Offline, paired CLI performance and ANSI rendering evidence (stdlib only).

Run after building comparable optimized binaries; this script never builds them.
Cold means a fresh application cache, NOT a flushed filesystem/OS page cache.
No desktop mutation commands, native terminal graphics, or network are exercised.
"""
import argparse
import hashlib
import html
import itertools
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import statistics
import struct
import subprocess
import sys
import tempfile
import time
import unicodedata
import unittest
from unittest.mock import patch
import zlib

ROOT = Path(__file__).resolve().parents[1]
SGR = re.compile(r"\x1b\[([0-9;]*)m")
CONFIDENCE = .999
BOOTSTRAPS = 20000
NEW_WARM_MS, NEW_COLD_MS = 500., 3000.
BASE_COLORS = ["#000000", "#aa0000", "#00aa00", "#aa5500", "#0000aa", "#aa00aa",
               "#00aaaa", "#aaaaaa", "#555555", "#ff5555", "#55ff55", "#ffff55",
               "#5555ff", "#ff55ff", "#55ffff", "#ffffff"]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def percentile(values, fraction):
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)]


def bootstrap_bounds(before, after):
    n = len(before)
    tail = math.ceil(n * .95) - 1
    middle = lambda sample: (sample[(n - 1) // 2] + sample[n // 2]) / 2
    rng = random.Random(76)
    boot = {"median": [], "p95": []}
    for _ in range(BOOTSTRAPS):
        indices = rng.choices(range(n), k=n)
        old = sorted(before[i] for i in indices)
        new = sorted(after[i] for i in indices)
        boot["median"].append(middle(new) - middle(old))
        boot["p95"].append(new[tail] - old[tail])
    return {name: percentile(values, (1 - CONFIDENCE) / 2) for name, values in boot.items()}


def compare(before, after):
    """Paired median/p95 confidence around zero; no allowed slowdown budget."""
    if len(before) != len(after) or len(before) < 9:
        raise ValueError("at least nine matched timing observations are required")
    deltas = [a - b for b, a in zip(before, after)]
    median = statistics.median
    bounds = bootstrap_bounds(before, after)
    metrics = {}
    for name, measure in [("median", median), ("p95", lambda values: percentile(values, .95))]:
        blocks = [measure(after[i::3]) - measure(before[i::3]) for i in range(3)]
        lower = bounds[name]
        supported = [delta > 0 for delta in blocks]
        repeated = sum(supported) >= 2
        # Exact ties can obscure a p95 shift in small bootstrap samples. A
        # repeated shift with NO faster pair is still a slowdown, not noise
        # that may be waived. Real clock measurements rarely contain ties.
        monotone = all(delta >= 0 for delta in deltas) and measure(after) > measure(before)
        evidence = lower > 0 or monotone
        metrics[name] = {"delta_ms": measure(after) - measure(before),
                         "bootstrap_lower_ms": lower, "block_deltas_ms": blocks,
                         "supported_blocks": supported,
                         "monotone": monotone, "regression": repeated and evidence,
                         "inconclusive": evidence and not repeated}
    return {"before_median_ms": median(before), "after_median_ms": median(after),
            "before_p95_ms": percentile(before, .95), "after_p95_ms": percentile(after, .95),
            "paired_deltas_ms": deltas, "paired_median_delta_ms": median(deltas),
            "delta_percent": 100 * (median(after) - median(before)) / median(before),
            "metrics": metrics, "regression_threshold_ms": 0,
            "regression": any(value["regression"] for value in metrics.values()),
            "needs_confirmation": any(value["regression"] or value["inconclusive"] for value in metrics.values())}


def compare_cold(before, after):
    return compare(before, after)


def confirmation_verdict(initial, confirmation):
    if not initial["needs_confirmation"]:
        return "PASS"
    if confirmation is not None and any(initial["metrics"][name]["regression"] and
                                        confirmation["metrics"][name]["regression"] for name in initial["metrics"]):
        return "FAIL"
    # A noisy second batch cannot erase supported evidence from the first.
    return "INCONCLUSIVE"


def supports(command, code, stdout, stderr):
    if code == 0 and SGR.sub("", stdout.decode("utf-8", errors="replace")).startswith(f"theme {command}"):
        return True
    if code == 1 and f"unknown command '{command}'".encode() in stderr:
        return False
    raise ValueError(f"{command} capability probe returned an unexpected response (exit {code})")


def new_budget(warm, cold, external_library):
    return {"applied": not external_library, "warm_median_ms": NEW_WARM_MS, "cold_median_ms": NEW_COLD_MS,
            "exceeded": not external_library and (warm > NEW_WARM_MS or cold > NEW_COLD_MS)}


def cell_width(text):
    # Deterministic wcwidth approximation: ambiguous characters occupy one cell.
    # Native emoji/grapheme shaping is outside this pipe-rendering check.
    return sum(0 if unicodedata.combining(c) or unicodedata.category(c) in {"Mn", "Me", "Cf"}
               else 2 if unicodedata.east_asian_width(c) in {"W", "F"} else 1 for c in text)


def rendering(data, width):
    text = data.decode("utf-8", errors="replace")
    plain = SGR.sub("", text)
    controls = sorted({f"U+{ord(c):04X}" for c in plain if ord(c) < 32 and c != "\n" or ord(c) == 127})
    widths = [cell_width(line) for line in plain.splitlines()]
    return {"maximum_cells": max(widths, default=0),
            "overflow_lines": [{"line": i + 1, "cells": size} for i, size in enumerate(widths) if size > width],
            "unexpected_controls": controls,
            "color_sequences": len(re.findall(r"\x1b\[(?:38|48);", text))}


def visible_colors(text):
    return [tuple(map(int, match.group(1, 2, 3))) for match in
            re.finditer(r"\x1b\[48;2;(\d+);(\d+);(\d+)m([^\x1b]*)", text)
            if match.group(4).strip("\r\n") and all(0 <= int(v) <= 255 for v in match.group(1, 2, 3))]


def semantic_errors(command, data, oracle):
    """Independent fixture facts; accepting fewer rows/work is never faster."""
    raw = data.decode("utf-8", errors="replace")
    plain = SGR.sub("", raw)
    compact = re.sub(r"\s+", "", plain)
    errors = []
    require = lambda condition, reason: errors.append(reason) if not condition else None
    swatches = min(8, max(2, (oracle["columns"] - 4) // 5))
    require(bool(plain.strip()), "empty visible output")
    for label in {"bare": ["ApplyCommands:", "LibraryCommands:", "InfoCommands:", "Usage:", "GlobalFlags", "COLORSCHEME", "TERMINAL", "THEMECLI"],
                  "help": ["ApplyCommands:", "LibraryCommands:", "InfoCommands:", "Usage:", "GlobalFlags", "COLORSCHEME", "TERMINAL", "THEMECLI"],
                  "version": ["version:", "github:https://github.com/snaraj/theme", "maintainer:SamuelNaranjo"],
                  "list": ["wallpapers", "TITLE", "COLORSCHEME"],
                  "list-v": ["wallpapers", "TITLE", "COLORSCHEME", "SOURCE", "FORMAT", "SIZE", "ADDED"],
                  "preview": ["TITLE", "FORMAT", "SIZE", "COLORSCHEME", "LOCATION"],
                  "search-metadata": ["search:landscape"], "search-color": ["search:blue"],
                  "browse-all": ["Wallpaperbrowser", "matches", "Query:allwallpapers"],
                  "browse-filter": ["Wallpaperbrowser", "matches", "Query:allwallpapers"],
                  "index": ["inspected", "palette(s)available", "cached", "metadatarecord(s)"]}[command]:
        require(label in compact, f"missing {label}")
    if command in ("bare", "help", "version"):
        require(bool(re.search(r"v\d+\.\d+\.\d+", compact)), "missing version value")
    if command in ("bare", "help"):
        for word in ("set", "random", "unsplash", "get", "list", "preview", "search", "rename", "remove", "status", "update", "version"):
            require(bool(re.search(r"^\s*" + word + r"(?:\W|$)", plain, re.M)), f"missing help command {word}")
        expected = {tuple(bytes.fromhex(color[1:])) for color in BASE_COLORS[:4]}
        require(expected <= set(visible_colors(raw)), "missing configured header palette colors")
    if command == "preview":
        require(re.sub(r"\s+", "", oracle["preview_name"]) in compact, "wrong or missing selected preview image")
        require(len(visible_colors(raw)) >= swatches and len(set(visible_colors(raw))) >= 2, "missing preview palette")
    if not oracle["synthetic"]:
        return errors
    count = oracle["images"]
    names = {f"fixture-{i:03d}" for i in range(count)}
    rows = set(re.findall(r"fixture-\d+", compact))
    if command in ("bare", "help"):
        require("fixture-000" in compact, "wrong current wallpaper in header")
    if command == "preview":
        fact = oracle["files"]["fixture-000"]
        require(f"{fact['width']}x{fact['height']}({fact['display_bytes']})" in compact and "png" in compact,
                "missing selected image format/dimensions/byte size")
    if command in ("list", "list-v", "search-metadata", "search-color"):
        hits = (count + 1) // 2 if command == "search-metadata" else count
        shown = min(hits, 12 if command.startswith("search") else 10)
        eligible = {f"fixture-{i:03d}" for i in range(0, count, 2)} if command == "search-metadata" else names
        require(len(rows) == shown and rows <= eligible, f"expected {shown} distinct valid fixture rows")
        row_starts = list(re.finditer(r"fixture-\d+", raw))
        require(len(row_starts) == shown, "missing or duplicated table rows")
        for i, start in enumerate(row_starts):
            row = raw[start.end():row_starts[i + 1].start() if i + 1 < len(row_starts) else len(raw)]
            require(len(visible_colors(row)) >= swatches and len(set(visible_colors(row))) >= 2,
                    "table row lost its visible multicolor palette")
            if command == "list-v":
                fact = oracle["files"].get(start.group())
                row_text = re.sub(r"\s+", "", SGR.sub("", row))
                require(fact is not None and "png" + fact["display_bytes"] in row_text,
                        "verbose row lost its actual format/byte size")
                require(bool(re.search(r"\d{4}-\d{2}-\d{2}", row_text)), "verbose row lost date metadata")
        if command == "list-v":
            require(compact.count("png") >= shown and len(re.findall(r"\d{4}-\d{2}-\d{2}", compact)) >= shown,
                    "verbose rows lost format/date metadata")
        if command in ("list", "list-v") and shown < count:
            require(f"newest{shown}of{count}" in compact and "themelist-n<count>,or--all" in compact,
                    "missing or incorrect truncated list total/more footer")
        if command.startswith("search"):
            footer = f"{shown}of{hits}matchesshown" if hits > shown else f"{hits}of{count}wallpapersmatch"
            require(footer in compact, "incorrect search result/total count")
            fact = "shape:landscape" if command == "search-metadata" else "colors:"
            require(compact.count(fact) >= shown, "search rows lost matching metadata")
    if command.startswith("browse"):
        require(f"{count}matches" in compact and len(rows) == min(count, 6) and rows <= names,
                "browser lost matches or first-page fixture paths")
        require("page1/" + str(math.ceil(count / 6)) in compact, "incorrect browser pagination")
    if command == "index":
        require(f"inspected{count}wallpaper(s)" in compact and f"{count}palette(s)available" in compact,
                "index did not inspect/prepare the complete fixture")
        require(f"cached{count}of{count}metadatarecord(s)" in compact, "index did not persist the complete fixture")
    return sorted(set(errors))


def palette_content(command, data, columns):
    raw = data.decode("utf-8", errors="replace")
    limit = min(8, max(2, (columns - 4) // 5))
    if command in ("list", "list-v", "search-metadata", "search-color"):
        rows = list(re.finditer(r"fixture-\d+", raw))
        return {row.group(): visible_colors(raw[row.end():rows[i + 1].start() if i + 1 < len(rows) else len(raw)])[:limit]
                for i, row in enumerate(rows)}
    return {}


def changed_palettes(before, after):
    # Tie ordering may legitimately select different equally ranked rows;
    # the independent row/count oracle covers completeness in that case.
    return sorted(name for name in before.keys() & after.keys() if before[name] != after[name])


def indexed_color(number):
    if number < 16:
        return BASE_COLORS[number]
    if number < 232:
        n = number - 16
        levels = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (levels[n // 36], levels[n // 6 % 6], levels[n % 6])
    return "#%02x%02x%02x" % ((8 + (number - 232) * 10,) * 3)


def ansi_html(data):
    """Render actual SGR colors/styles; escape all text, never execute output."""
    text = data.decode("utf-8", errors="replace")
    state, parts, position = {}, [], 0
    for match in list(SGR.finditer(text)) + [None]:
        end = match.start() if match else len(text)
        raw = text[position:end]
        raw = "".join(c if ord(c) >= 32 or c == "\n" else f"\\x{ord(c):02x}" for c in raw)
        escaped = html.escape(raw)
        style = ";".join(f"{key}:{value}" for key, value in state.items())
        parts.append(f'<span style="{style}">{escaped}</span>' if style else escaped)
        if match is None:
            break
        values = [int(v or "0") for v in match.group(1).split(";")]
        i = 0
        while i < len(values):
            code = values[i]
            if code == 0:
                state.clear()
            elif code in (1, 2, 3, 4):
                key, value = {1: ("font-weight", "bold"), 2: ("opacity", ".65"),
                              3: ("font-style", "italic"), 4: ("text-decoration", "underline")}[code]
                state[key] = value
            elif code in (22, 23, 24, 39, 49):
                for key in {22: ["font-weight", "opacity"], 23: ["font-style"],
                            24: ["text-decoration"], 39: ["color"], 49: ["background-color"]}[code]:
                    state.pop(key, None)
            elif 30 <= code <= 37 or 90 <= code <= 97 or 40 <= code <= 47 or 100 <= code <= 107:
                foreground = 30 <= code <= 37 or 90 <= code <= 97
                state["color" if foreground else "background-color"] = BASE_COLORS[code % 10 + (8 if code >= 90 else 0)]
            elif code in (38, 48) and i + 2 < len(values):
                color = None
                if values[i + 1] == 5 and 0 <= values[i + 2] <= 255:
                    color, i = indexed_color(values[i + 2]), i + 2
                elif values[i + 1] == 2 and i + 4 < len(values) and all(0 <= v <= 255 for v in values[i + 2:i + 5]):
                    color, i = "#%02x%02x%02x" % tuple(values[i + 2:i + 5]), i + 4
                if color:
                    state["color" if code == 38 else "background-color"] = color
            i += 1
        position = match.end()
    return "".join(parts)


def png(number, width, height):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    pixels = bytearray()
    for y in range(height):
        pixels.append(0)
        for x in range(width):
            pixels.extend(((x * 3 + number * 17) % 256, (y * 5 + number * 11) % 256,
                           (128 + x + y + number * 7) % 256))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(pixels), 6)) + chunk(b"IEND", b""))


class Harness:
    def __init__(self, args):
        if args.built_artifacts and args.library:
            raise ValueError("--built-artifacts requires the controlled synthetic fixture; --library stays strict")
        self.args = args
        self.harness_sha256 = digest(Path(__file__).read_bytes())
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        if any(self.output.iterdir()):
            raise ValueError("--output must be empty; existing evidence is never overwritten")
        private = ROOT / "target/test-tmp"
        private.mkdir(parents=True, exist_ok=True)
        self.fixture = Path(tempfile.mkdtemp(prefix="performance-", dir=private))
        self.binaries = {label: path.resolve(strict=True) for label, path in [("before", args.before), ("after", args.after)]}
        self.binary_info = {}
        for label, path in self.binaries.items():
            data = path.read_bytes()
            self.binary_info[label] = {"path": str(path), "sha256": digest(data),
                "native_header": data[:4] in (b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf",
                                              b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xce", b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf")}
        self.records, self.views, self.failures, self.palettes, self.inconclusive = [], {}, [], {}, []
        self.library = args.library.resolve(strict=True) if args.library else self.fixture / "library"
        if not args.library:
            self.library.mkdir()
            for i in range(args.images):
                size = (256, 160) if i % 2 == 0 else (144, 240)
                image = self.library / f"fixture-{i:03d}.png"
                image.write_bytes(png(i, *size))
                os.utime(image, (1700000000, 1700000000))
        files = sorted(p for p in self.library.rglob("*") if p.is_file() and not p.is_symlink()
                       and p.suffix.lower() in {".png", ".jpg", ".jpeg", ".webp"})
        if not files:
            raise ValueError("library has no supported fixture images")
        self.commands = {"bare": [], "help": ["help"], "version": ["--version"], "list": ["list"],
                         "list-v": ["list", "-v"], "preview": ["preview", str(files[0])],
                         "search-metadata": ["search", "landscape", "-n", "12"],
                         "search-color": ["search", "blue", "-n", "12"],
                         "browse-all": ["browse", "--all"],
                         "browse-filter": ["browse", "--all", "--min-contrast", "1"], "index": ["index"]}
        if args.commands:
            unknown = set(args.commands) - self.commands.keys()
            if unknown:
                raise ValueError(f"unknown benchmark commands: {', '.join(sorted(unknown))}")
            self.commands = {name: argv for name, argv in self.commands.items() if name in args.commands}
        self.variants = {command: ["before", "after"] for command in self.commands}
        self.capabilities = {}
        bindir = self.fixture / "bin"
        bindir.mkdir()
        wallpaper = bindir / "wallpaper"
        wallpaper.write_text('#!/bin/sh\n[ "$#" -eq 1 ] && [ "$1" = get ] || exit 64\nprintf "%s\\n" "$THEME_PERF_WALLPAPER"\n')
        wallpaper.chmod(0o700)
        self.env = {"PATH": f"{bindir}:/usr/bin:/bin", "LANG": "C", "LC_ALL": "C", "TZ": "UTC",
                    "TERM": "xterm-256color", "THEME_NO_UPDATE_CHECK": "1",
                    "THEME_WALLPAPER_DIR": str(self.library), "THEME_PERF_WALLPAPER": str(files[0]),
                    "KITTY_WINDOW_ID": ""}
        self.library_info = {"path": str(self.library), "synthetic": not bool(args.library),
                             "images": len(files), "manifest_sha256": digest("\n".join(
                                 f"{p.relative_to(self.library)} {p.stat().st_size}" for p in files).encode())}
        facts = {}
        if not args.library:
            for path in files:
                byte_count = path.stat().st_size
                width, height = struct.unpack(">II", path.read_bytes()[16:24])
                facts[path.stem] = {"bytes": byte_count, "width": width, "height": height,
                                    "display_bytes": f"{byte_count / 1024:.0f}K"}
        self.library_info["files"] = facts
        self.oracle = self.library_info | {"preview_name": files[0].name}

    def environment(self, command, variant, iteration=0, width=80):
        root = self.fixture / command / variant / str(iteration)
        for child in ("config/kitty", "cache", "tmp"):
            (root / child).mkdir(parents=True, exist_ok=True)
        kitty = root / "config/kitty"
        (kitty / "kitty.conf").write_text("include current-theme.conf\n")
        palette = kitty / "fixture-palette.conf"
        (kitty / "current-theme.conf").write_text(f"include {palette}\n")
        palette.write_text("background #111111\nforeground #dddddd\n" +
            "".join(f"color{i} {color}\n" for i, color in enumerate(BASE_COLORS)))
        return self.env | {"CONFIG_DIR": str(root / "config"), "KITTY_CONFIG_DIRECTORY": str(kitty),
                           "THEME_CACHE_DIR": str(root / "cache"), "TMPDIR": str(root / "tmp"), "COLUMNS": str(width)}

    def run(self, command, variant, phase, sample, environment, arguments=None, allow_failure=False):
        argv = [str(self.binaries[variant])] + (self.commands[command] if arguments is None else arguments)
        start = time.perf_counter_ns()
        try:
            result = subprocess.run(argv, env=environment, cwd=self.fixture, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=self.args.timeout)
            stdout, stderr, code = result.stdout, result.stderr, result.returncode
        except subprocess.TimeoutExpired as error:
            stdout, stderr, code = error.stdout or b"", error.stderr or b"", "timeout"
        except OSError as error:
            stdout, stderr, code = b"", str(error).encode(), "spawn-error"
        elapsed = (time.perf_counter_ns() - start) / 1_000_000
        stem = f"{command}/{variant}-{phase}-{sample}"
        destination = self.output / stem
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.with_suffix(".stdout.ansi").write_bytes(stdout)
        destination.with_suffix(".stderr.txt").write_bytes(stderr)
        record = {"command": command, "variant": variant, "phase": phase, "sample": sample,
                  "argv": argv, "elapsed_ms": elapsed, "exit_code": code,
                  "stdout": stem + ".stdout.ansi", "stderr": stem + ".stderr.txt", "stdout_sha256": digest(stdout)}
        self.records.append(record)
        if code != 0 and not allow_failure:
            self.failures.append(f"{stem}: exit {code}")
        if arguments is None and code == 0:
            record["semantic_errors"] = semantic_errors(command, stdout, self.oracle | {"columns": int(environment["COLUMNS"])})
            for reason in record["semantic_errors"]:
                failure = f"{command}/{variant}: semantic: {reason}"
                if failure not in self.failures:
                    self.failures.append(failure)
            record["palette_content"] = palette_content(command, stdout, int(environment["COLUMNS"]))
            pair = self.palettes.setdefault((command, phase, sample), {})
            pair[variant] = record["palette_content"]
            if "before" in pair and "after" in pair:
                changed = changed_palettes(pair["before"], pair["after"])
                failure = f"{command}: shared-row palette content differs"
                if changed and failure not in self.failures:
                    self.failures.append(failure)
        return elapsed, stdout

    def discover(self):
        for feature, commands in [("browse", ["browse-all", "browse-filter"]), ("index", ["index"])]:
            commands = [command for command in commands if command in self.commands]
            if not commands:
                continue
            found = {}
            for variant in self.binaries:
                _, stdout = self.run(f"probe-{feature}", variant, "capability", 0,
                    self.environment(f"probe-{feature}", variant), [feature, "--help"], allow_failure=True)
                record = self.records[-1]
                try:
                    available = supports(feature, record["exit_code"], stdout, (self.output / record["stderr"]).read_bytes())
                except ValueError as error:
                    available = False
                    self.failures.append(f"{variant}: {error}")
                found[variant] = {"supported": available, "probe": record}
            self.capabilities[feature] = found
            if not found["after"]["supported"]:
                self.failures.append(f"candidate lacks required {feature} capability")
            for command in commands:
                self.variants[command] = (["before", "after"] if found["before"]["supported"] else ["after"])
                if not found["after"]["supported"]:
                    self.variants[command] = []

    def timing_batch(self, command, variants, phase, count, environments, cold=False):
        observed = {variant: [] for variant in variants}
        for sample in range(count):
            for variant in variants[::1 if sample % 2 == 0 else -1]:
                environment = environments[variant]
                if cold:
                    # Confirmation never reuses an initial cold cache.
                    iteration = sample if phase == "cold" else f"{phase}-{sample}"
                    environment = self.environment(command, variant, iteration)
                observed[variant].append(self.run(command, variant, phase, sample, environment)[0])
        return observed

    def measure(self):
        self.discover()
        comparisons = {}
        for command in self.commands:
            variants = self.variants[command]
            if not variants:
                continue
            environments = {variant: self.environment(command, variant) for variant in variants}
            cold = self.timing_batch(command, variants, "cold", self.args.cold_samples, environments, cold=True)
            for sample in range(2):
                for variant in variants:
                    self.run(command, variant, "warmup", sample, environments[variant])
            warm = self.timing_batch(command, variants, "warm", self.args.samples, environments)
            paired = "before" in variants
            result = compare(warm["before"], warm["after"]) if paired else {
                "before_median_ms": None, "before_p95_ms": None, "delta_percent": None, "regression": False,
                "after_median_ms": statistics.median(warm["after"]), "after_p95_ms": percentile(warm["after"], .95)}
            result["status"] = "PAIRED" if paired else "NEW — no baseline"
            result["sample_counts"] = {"warm": {variant: len(values) for variant, values in warm.items()},
                                       "cold": {variant: len(values) for variant, values in cold.items()}}
            result["cold"] = {variant: {"median_ms": statistics.median(values), "p95_ms": percentile(values, .95),
                                       "raw_ms": values} for variant, values in cold.items()}
            result["cold_comparison"] = compare_cold(cold["before"], cold["after"]) if paired else None
            comparisons[command] = result
            result["confirmation"], result["phase_verdicts"] = {}, {}
            if paired:
                for phase, initial, count in [("cold", result["cold_comparison"], self.args.cold_samples),
                                               ("warm", result, self.args.samples)]:
                    confirmation = None
                    if initial["needs_confirmation"]:
                        observed = self.timing_batch(command, variants, f"{phase}-confirm", count, environments, cold=phase == "cold")
                        confirmation = compare(observed["before"], observed["after"])
                        result["confirmation"][phase] = {"comparison": confirmation, "raw_ms": observed,
                            "sample_counts": {variant: len(values) for variant, values in observed.items()}}
                    result["phase_verdicts"][phase] = confirmation_verdict(initial, confirmation)
            if not paired:
                result["absolute_budget"] = new_budget(result["after_median_ms"], result["cold"]["after"]["median_ms"], bool(self.args.library))
                if result["absolute_budget"]["exceeded"]:
                    self.failures.append(f"{command}: new command exceeds synthetic fixture stall budget")
            for width in (25, 80, 120):
                views = {}
                for variant in variants:
                    _, data = self.run(command, variant, f"render-{width}", 0, environments[variant] | {"COLUMNS": str(width)})
                    views[variant] = rendering(data, width)
                    self.views[(command, width, variant)] = data
                old = views.get("before", {"maximum_cells": width, "overflow_lines": [], "color_sequences": 0})
                new = views["after"]
                if (new["maximum_cells"] > max(width, old["maximum_cells"]) or
                        len(new["overflow_lines"]) > len(old["overflow_lines"]) or new["unexpected_controls"] or
                        old["color_sequences"] and not new["color_sequences"]):
                    self.failures.append(f"{command} width {width}: rendering regression")
                result.setdefault("rendering", {})[str(width)] = views
            baseline = f"{result['before_median_ms']:.2f} → " if paired else "NEW, no baseline: "
            print(f"{command}: {baseline}{result['after_median_ms']:.2f} ms {result['phase_verdicts']}", flush=True)
        return comparisons

    def finalize_timing(self, comparisons):
        # Identity is evidence about executable bytes, never a statistical
        # PASS. Re-read both files after the complete measurement/rendering run.
        final = {}
        for label, path in self.binaries.items():
            try:
                final[label] = digest(path.read_bytes())
            except OSError as error:
                final[label] = None
                self.failures.append(f"{label} binary unreadable after measurement: {error}")
            if final[label] != self.binary_info[label]["sha256"]:
                self.failures.append(f"{label} binary changed during measurement")
        initial = {label: info["sha256"] for label, info in self.binary_info.items()}
        identical = len(set(initial.values()) | set(final.values())) == 1
        eligible = (self.args.built_artifacts and not self.args.library and
                    all(info["native_header"] for info in self.binary_info.values()))
        if eligible and identical:
            for feature, probes in self.capabilities.items():
                if probes["before"]["supported"] != probes["after"]["supported"]:
                    self.failures.append(f"identical binaries disagree on {feature} capability")
                    eligible = False
        self.binary_identity = {"initial_sha256": initial, "final_sha256": final,
                                "identical": identical, "built_artifacts_requested": self.args.built_artifacts,
                                "eligible": eligible}
        self.timing_failures = []
        for command, result in comparisons.items():
            result["timing_gate_verdicts"] = {}
            for phase, statistical in result["phase_verdicts"].items():
                decision = "IDENTICAL_BINARY" if eligible and identical else statistical
                result["timing_gate_verdicts"][phase] = decision
                if decision not in ("PASS", "IDENTICAL_BINARY"):
                    message = f"{command}: {phase} {decision} after independent confirmation"
                    self.timing_failures.append(message)
                    if decision == "INCONCLUSIVE":
                        self.inconclusive.append(message)

    def report(self, comparisons):
        self.finalize_timing(comparisons)
        controls = ["Wall-clock subprocess execution includes captured stdout/stderr; file artifact writes are outside timings.",
                    "Fresh application caches for cold samples; OS/filesystem page caches are not flushed.",
                    "Each command/binary has its own cache; two recorded warmups precede paired alternating warm samples and are excluded from comparisons.",
                    "HOME and ambient environment are omitted, never reassigned; config/cache/Kitty/TMPDIR paths are isolated.",
                    "THEME_NO_UPDATE_CHECK=1; PATH contains a read-only wallpaper-get fixture stub plus /usr/bin:/bin.",
                    "Cache derivation/persistence is enabled. No desktop-mutating verbs or native Kitty graphics are invoked.",
                    "A known included header palette is configured. Every run must retain command text, identities, metadata, and visible palette swatches.",
                    "Synthetic fixture row/search/browser/index totals are checked independently; external-library checks are structural and verify the selected preview identity.",
                    "Shared list/search fixture rows must retain their actual RGB palette content; previews independently require image identity/metadata/swatches but may change their final palette.",
                    "Width uses stdlib Unicode cell estimates; native font/emoji shaping is not covered.",
                    "Cold and warm costs and existing width overflows are reported; gates cover repeated timing and new/worse rendering regressions.",
                    "browse/index support is probed through --help. New commands have candidate-only timings, never before/after speed claims.",
                    "New-command synthetic fixture stall budgets: warm median 500 ms, cold median 3000 ms. Budgets are skipped with --library.",
                    "New-command rendering must fit 25/80/120 columns. Once the baseline supports the command, normal paired gates apply."]
        controls += ["A supported initial timing shift triggers an independent alternating confirmation batch with the same sample count; cold confirmation uses fresh caches.",
                     "For different executable bytes, matching supported metric regressions in both batches FAIL. An initial signal without a matching confirmation is INCONCLUSIVE and blocks CI; it never becomes a statistical PASS.",
                     "IDENTICAL_BINARY is restricted to --built-artifacts with the controlled synthetic fixture, native executable headers and matching before/after hashes for both executables. The caller must supply comparable direct builds; headers alone are not build provenance. Standalone runs stay strict by default.",
                     "In IDENTICAL_BINARY mode, timing observations and statistical verdicts remain diagnostics: this is no code-performance comparison or speed claim. All execution, semantic, capability and rendering errors still block CI.",
                     "With nine cold samples the nearest-rank p95 is the maximum observation; every observation is retained."]
        if self.args.commands:
            controls.append("Explicit command subset: " + ", ".join(self.commands) + "; this is not a complete CI matrix run.")
        failures = self.failures + self.timing_failures
        revisions = {label: os.environ.get(key, "unknown") for label, key in
                     [("before", "THEME_PERF_BASE_SHA"), ("after", "THEME_PERF_HEAD_SHA")]}
        verdict = ("FAIL" if self.failures or any(failure not in self.inconclusive for failure in self.timing_failures)
                   else "INCONCLUSIVE" if self.inconclusive else "PASS")
        report = {"schema": 2, "platform": {"system": platform.system(), "release": platform.release(),
                  "machine": platform.machine(), "python": platform.python_version()},
                  "harness_sha256": self.harness_sha256,
                  "binaries": self.binary_info, "binary_identity": self.binary_identity,
                  "revisions": revisions, "capabilities": self.capabilities,
                  "selected_commands": list(self.commands), "complete_matrix": not bool(self.args.commands),
                  "library": self.library_info, "fixture": str(self.fixture), "controls": controls,
                  "thresholds": {"allowed_slowdown_ms": 0, "bootstrap_confidence": CONFIDENCE, "bootstrap_resamples": BOOTSTRAPS,
                  "warm_samples": self.args.samples, "cold_samples": self.args.cold_samples},
                  "comparisons": comparisons, "runs": self.records, "failures": failures,
                  "inconclusive": self.inconclusive, "verdict": verdict, "passed": verdict == "PASS"}
        (self.output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
        lines = ["# CLI performance and rendering", "", f"Platform: `{platform.system()} {platform.machine()}`. "
                 f"Result: **{verdict}**.", "",
                 "| Command | Warm before → after (ms) | p95 before → after (ms) | Paired delta | Cold before → after (ms) | Warm / cold timing gate (statistics) |",
                 "|---|---:|---:|---:|---:|---|"]
        for name, result in comparisons.items():
            if result["status"] != "PAIRED":
                lines.append(f"| {name} **NEW** | no baseline → {result['after_median_ms']:.2f} | "
                             f"no baseline → {result['after_p95_ms']:.2f} | not comparable | "
                             f"no baseline → {result['cold']['after']['median_ms']:.2f} | NEW / no baseline |")
                continue
            lines.append(f"| {name} | {result['before_median_ms']:.2f} → {result['after_median_ms']:.2f} | "
                         f"{result['before_p95_ms']:.2f} → {result['after_p95_ms']:.2f} | {result['delta_percent']:+.1f}% | "
                         f"{result['cold']['before']['median_ms']:.2f} → {result['cold']['after']['median_ms']:.2f} | "
                         + " / ".join(f"{result['timing_gate_verdicts'][phase]} ({result['phase_verdicts'][phase]})"
                                      for phase in ("warm", "cold")) + " |")
        lines += ["", "The table shows the initial batch. All initial and confirmation timings, binary/harness SHA-256, fixture facts, width findings, and capture paths: [results.json](results.json).",
                  "Actual ANSI colors/output: [rendering.html](rendering.html).", "", "Controls:", ""]
        lines += [f"- {line}" for line in controls]
        lines += ["", "Cold and warm median/p95 signals use paired bootstrap 99.9% confidence around zero "
                  "over the full batch plus positive metric shifts in at least two of three interleaved blocks. "
                  "A repeated metric shift with no faster pair also qualifies, including exact-tie tail cases. "
                  "Block point estimates are not themselves confidence intervals. A supported signal without two "
                  "positive blocks remains inconclusive. Independent confirmation determines statistical verdicts. "
                  "Different executable bytes retain the zero-slack gate; identical bytes are explicitly classified separately.", "",
                  f"Requested samples per supported binary/command: {self.args.samples} warm, {self.args.cold_samples} cold. "
                  "Actual counts are recorded per case in results.json.", ""]
        lines += [f"- {failure}" for failure in failures]
        markdown = "\n".join(lines) + "\n"
        (self.output / "report.md").write_text(markdown)
        if os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
                summary.write(markdown.replace("[results.json](results.json)", "`results.json`")
                              .replace("[rendering.html](rendering.html)", "`rendering.html`"))
        panels = []
        for command in comparisons:
            for width in (25, 80, 120):
                panels.append(f"<h2>{html.escape(command)} · {width} columns</h2><div class=pair>")
                if "before" not in self.variants[command]:
                    panels.append("<p>NEW — no baseline capability; candidate only, not a speed comparison.</p>")
                for variant in self.variants[command]:
                    panels.append(f"<section><h3>{variant}</h3><pre style=width:{width}ch>"
                                  + ansi_html(self.views[(command, width, variant)]) + "</pre></section>")
                panels.append("</div>")
        (self.output / "rendering.html").write_text("<!doctype html><meta charset=utf-8><title>Theme CLI rendering evidence</title>"
            "<style>body{background:#202124;color:#eee;font:14px system-ui;margin:24px}.pair{display:flex;gap:24px;overflow:auto}"
            "pre{font:12px/1.4 monospace;background:#111;color:#ddd;padding:12px;white-space:pre;overflow:visible}"
            "section{flex:none}h2{margin-top:40px}</style><h1>Actual captured ANSI output</h1>"
            "<p>Pipe rendering, no native Kitty images. Overflow remains visible. See results.json for cell-width findings.</p>"
            + "".join(panels))
        return report


class SelfTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.dict(os.environ))
        os.environ.pop("GITHUB_STEP_SUMMARY", None)

    def identity_fixture(self, before=b"\x7fELForiginal", after=b"\x7fELForiginal", built_artifacts=True):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        binaries = [directory / name for name in ("before", "after")]
        for path, data in zip(binaries, (before, after)):
            path.write_bytes(data)
        harness = Harness(argparse.Namespace(before=binaries[0], after=binaries[1],
            output=directory / "output", library=None, images=2, commands=["version"],
            samples=21, cold_samples=9, timeout=1, built_artifacts=built_artifacts))
        # These are diagnostic verdicts supplied to finalization, not fabricated
        # benchmark observations. The comparator's own tests cover statistics.
        comparisons = {"version": {"phase_verdicts": {"cold": "INCONCLUSIVE", "warm": "FAIL"}}}
        return harness, comparisons

    def test_identical_bytes_preserve_statistical_diagnostics(self):
        harness, comparisons = self.identity_fixture()
        original = dict(comparisons["version"]["phase_verdicts"])
        harness.finalize_timing(comparisons)
        self.assertTrue(harness.binary_identity["identical"])
        self.assertEqual(comparisons["version"]["phase_verdicts"], original)
        self.assertEqual(comparisons["version"]["timing_gate_verdicts"],
                         {"cold": "IDENTICAL_BINARY", "warm": "IDENTICAL_BINARY"})
        self.assertEqual(harness.timing_failures, [])
        self.assertEqual(harness.failures, [])

    def test_different_bytes_keep_zero_slack_verdicts(self):
        harness, comparisons = self.identity_fixture(after=b"\x7fELFchanged")
        harness.finalize_timing(comparisons)
        self.assertFalse(harness.binary_identity["identical"])
        self.assertEqual(comparisons["version"]["timing_gate_verdicts"],
                         comparisons["version"]["phase_verdicts"])
        self.assertEqual(len(harness.timing_failures), 2)
        self.assertEqual(len(harness.inconclusive), 1)

    def test_identity_mode_requires_explicit_controlled_native_builds(self):
        # This caller owns the comparable-build assertion. Losing its opt-in
        # would silently restore flaky comparisons of identical CI artifacts.
        self.assertIn("--built-artifacts", (ROOT / "tests/performance-ci.sh").read_text())
        for options in ({"built_artifacts": False}, {"before": b"script", "after": b"script"}):
            with self.subTest(options=options):
                harness, comparisons = self.identity_fixture(**options)
                harness.finalize_timing(comparisons)
                self.assertTrue(harness.binary_identity["identical"])
                self.assertFalse(harness.binary_identity["eligible"])
                self.assertEqual(len(harness.timing_failures), 2)
        harness, _ = self.identity_fixture()
        harness.args.library = harness.library
        with self.assertRaisesRegex(ValueError, "--library stays strict"):
            Harness(harness.args)

    def test_identity_denies_contradictory_capabilities(self):
        harness, comparisons = self.identity_fixture()
        harness.capabilities = {"index": {"before": {"supported": False}, "after": {"supported": True}}}
        harness.finalize_timing(comparisons)
        self.assertTrue(harness.binary_identity["identical"])
        self.assertFalse(harness.binary_identity["eligible"])
        self.assertTrue(harness.failures)
        self.assertEqual(len(harness.timing_failures), 2)

    def test_expected_control_failures_do_not_pollute_ci_summary(self):
        directory = Path(self.enterContext(tempfile.TemporaryDirectory()))
        summary = directory / "summary.md"
        summary.write_text("earlier CI steps\n")
        os.environ["GITHUB_STEP_SUMMARY"] = str(summary)
        result = unittest.TestResult()
        SelfTests("test_identical_binary_cannot_hide_other_failure_classes").run(result)
        self.assertTrue(result.wasSuccessful(), result.errors + result.failures)
        self.assertEqual(summary.read_text(), "earlier CI steps\n")
        self.assertEqual(os.environ["GITHUB_STEP_SUMMARY"], str(summary))
        harness, _ = self.identity_fixture()
        harness.report({})
        self.assertIn("Result: **PASS**", summary.read_text())

    def test_identical_path_sensitive_wrapper_delay_still_fails(self):
        script = (b'#!/bin/sh\ncase "$0" in */after) sleep 0.03;; esac\n'
                  b'printf "version: v1.2.3\\ngithub: https://github.com/snaraj/theme\\nmaintainer: Samuel Naranjo\\n"\n')
        harness, _ = self.identity_fixture(script, script, built_artifacts=False)
        for path in harness.binaries.values():
            path.chmod(0o700)
        code = main(["--before", str(harness.binaries["before"]), "--after", str(harness.binaries["after"]),
                     "--output", str(harness.output), "--commands", "version", "--images", "2", "--samples", "21"])
        report = json.loads((harness.output / "results.json").read_text())
        self.assertTrue(code)
        self.assertTrue(report["binary_identity"]["identical"])
        self.assertFalse(report["binary_identity"]["eligible"])
        self.assertEqual(report["verdict"], "FAIL")
        timing = report["comparisons"]["version"]
        self.assertEqual(timing["timing_gate_verdicts"], timing["phase_verdicts"])
        self.assertEqual(timing["timing_gate_verdicts"]["warm"], "FAIL")

    def test_actual_timeout_is_unconditional(self):
        script = b"#!/bin/sh\nexec sleep 1\n"
        harness, _ = self.identity_fixture(script, script)
        harness.binaries["after"].chmod(0o700)
        harness.args.timeout = .01
        harness.run("version", "after", "control", 0, harness.environment("version", "after"))
        report = harness.report({})
        self.assertEqual(report["runs"][0]["exit_code"], "timeout")
        self.assertEqual(report["verdict"], "FAIL")

    def test_identity_requires_both_initial_and_both_final_hashes(self):
        cases = [(b"original", b"original", {"before": b"changed"}),
                 (b"original", b"original", {"after": b"changed"}),
                 (b"original", b"original", {"before": b"changed", "after": b"changed"}),
                 (b"changed", b"original", {"before": b"original"}),
                 (b"original", b"changed", {"after": b"original"})]
        for before, after, changes in cases:
            with self.subTest(changes=changes, before=before, after=after):
                harness, comparisons = self.identity_fixture(b"\x7fELF" + before, b"\x7fELF" + after)
                for label, data in changes.items():
                    harness.binaries[label].write_bytes(b"\x7fELF" + data)
                harness.finalize_timing(comparisons)
                self.assertFalse(harness.binary_identity["identical"])
                self.assertTrue(harness.failures)
                self.assertEqual(len(harness.timing_failures), 2)

    def test_missing_binary_fails_and_report_survives(self):
        harness, _ = self.identity_fixture()
        harness.binaries["after"].unlink()
        report = harness.report({})
        self.assertEqual(report["verdict"], "FAIL")
        self.assertIsNone(report["binary_identity"]["final_sha256"]["after"])
        self.assertTrue((harness.output / "results.json").is_file())
        self.assertTrue((harness.output / "rendering.html").is_file())

    def test_identical_binary_cannot_hide_execution_or_semantic_errors(self):
        for executable in (b"not executable", b"#!/bin/sh\nexit 7\n", b"#!/bin/sh\nprintf empty\n"):
            with self.subTest(executable=executable):
                harness, _ = self.identity_fixture(executable, executable)
                harness.binaries["after"].chmod(0o700)
                harness.run("version", "after", "control", 0, harness.environment("version", "after"))
                report = harness.report({})
                self.assertTrue(report["binary_identity"]["identical"])
                self.assertEqual(report["verdict"], "FAIL")
                record = report["runs"][0]
                self.assertEqual(digest((harness.output / record["stdout"]).read_bytes()), record["stdout_sha256"])

    def test_identical_binary_cannot_hide_other_failure_classes(self):
        for failure in ("timeout", "capability", "palette", "rendering"):
            with self.subTest(failure=failure):
                harness, _ = self.identity_fixture()
                harness.failures.append(failure)
                report = harness.report({})
                self.assertTrue(report["binary_identity"]["identical"])
                self.assertEqual(report["verdict"], "FAIL")
                self.assertIn(failure, report["failures"])

    def test_regression_requires_repeatable_shift(self):
        self.assertTrue(compare([10.] * 21, [12.] * 21)["regression"])
        self.assertTrue(compare([1.] * 21, [1.01] * 21)["regression"])
        self.assertTrue(compare([1.] * 21, [math.nextafter(1., math.inf)] * 21)["regression"])
        self.assertTrue(compare([10.] * 21, [50.] * 7 + [10.] * 14)["metrics"]["p95"]["regression"])
        self.assertTrue(compare_cold([10.] * 9, [100., 9., 100.] * 3)["metrics"]["p95"]["regression"])
        self.assertFalse(compare([9., 10., 11.] * 7, [9., 10., 11.] * 7)["regression"])
        with self.assertRaises(ValueError):
            compare_cold([10.], [14.])

    def test_cold_tail_gate_is_not_defeated_by_ordering(self):
        # Every permutation of six slow and three faster observations must
        # remain supported, including the review's contiguous ordering.
        for faster in itertools.combinations(range(9), 3):
            after = [9. if i in faster else 100. for i in range(9)]
            with self.subTest(faster=faster):
                self.assertTrue(compare_cold([10.] * 9, after)["metrics"]["p95"]["regression"])

    def test_confirmation_never_turns_supported_evidence_green(self):
        initial = compare([10.] * 21, [10.01] * 21)
        confirmed = compare([10.] * 21, [10.02] * 21)
        unchanged = compare([10.] * 21, [10.] * 21)
        self.assertEqual(confirmation_verdict(initial, confirmed), "FAIL")
        self.assertEqual(confirmation_verdict(initial, unchanged), "INCONCLUSIVE")
        self.assertEqual(confirmation_verdict(initial, None), "INCONCLUSIVE")
        self.assertEqual(confirmation_verdict(unchanged, None), "PASS")
        # A tail shift concentrated in one block also cannot pass silently.
        one_block = compare([10.] * 21, [50., 10., 10.] * 7)
        self.assertTrue(one_block["needs_confirmation"])
        self.assertEqual(confirmation_verdict(one_block, unchanged), "INCONCLUSIVE")

    def test_actual_color_and_escaped_text(self):
        rendered = ansi_html(b"\x1b[48;2;1;2;3m  \x1b[0m<script>")
        self.assertIn("background-color:#010203", rendered)
        self.assertIn("&lt;script&gt;", rendered)
        self.assertEqual(indexed_color(196), "#ff0000")

    def test_capability_probe_does_not_confuse_errors_with_absence(self):
        self.assertTrue(supports("browse", 0, b"theme browse [terms...]\n", b""))
        self.assertFalse(supports("browse", 1, b"general help", b"theme: unknown command 'browse'"))
        with self.assertRaises(ValueError):
            supports("browse", "timeout", b"", b"")
        with self.assertRaises(ValueError):
            supports("browse", 0, b"general help", b"")

    def test_new_command_budget_applies_only_to_synthetic_fixtures(self):
        self.assertFalse(new_budget(100., 1000., False)["exceeded"])
        self.assertTrue(new_budget(501., 1000., False)["exceeded"])
        self.assertTrue(new_budget(100., 3001., False)["exceeded"])
        self.assertFalse(new_budget(1000., 5000., True)["exceeded"])
        self.assertFalse(new_budget(1000., 5000., True)["applied"])

    def test_semantic_oracle_rejects_removed_work_even_with_color_escape(self):
        oracle = {"synthetic": True, "images": 4, "preview_name": "fixture-000.png", "columns": 80,
                  "files": {f"fixture-{i:03d}": {"bytes": 1024, "display_bytes": "1K", "width": 256,
                                                "height": 160} for i in range(64)}}
        palette = "".join("\x1b[48;2;%d;%d;%dm   \x1b[0m" % tuple(bytes.fromhex(c[1:])) for c in BASE_COLORS[:8])
        rows = lambda ids, suffix="": "\n".join(f"fixture-{i:03d} {palette} {suffix}" for i in ids)
        help_text = ("fixture-000 COLORSCHEME " + palette + " TERMINAL xterm THEME CLI v1.2.3\n"
                     "Apply Commands:\nLibrary Commands:\nInfo Commands:\nUsage:\nGlobal Flags\n" +
                     "\n".join("  " + c for c in ["set", "random", "unsplash", "get", "list", "preview", "search", "rename", "remove", "status", "update", "version"]))
        outputs = {"bare": help_text, "help": help_text,
                   "version": "version: v1.2.3\ngithub: https://github.com/snaraj/theme\nmaintainer: Samuel Naranjo",
                   "list": "wallpapers\nTITLE COLORSCHEME\n" + rows(range(4)),
                   "list-v": "wallpapers\nTITLE COLORSCHEME SOURCE FORMAT SIZE ADDED\n" + rows(range(4), "png 1K 2023-11-14"),
                   "preview": "TITLE fixture-000\nFORMAT png\nSIZE 256x160 (1K)\nCOLORSCHEME " + palette + "\nLOCATION /fixture-000.png",
                   "search-metadata": "search: landscape\n" + rows([0, 2], "shape: landscape") + "\n2 of 4 wallpapers match",
                   "search-color": "search: blue\n" + rows(range(4), "colors: blue") + "\n4 of 4 wallpapers match",
                   "browse-all": "Wallpaper browser | 4 matches | page 1/1\nQuery: all wallpapers\n" + rows(range(4)),
                   "index": "inspected 4 wallpaper(s); 4 palette(s) available\ncached 4 of 4 metadata record(s)"}
        outputs["browse-filter"] = outputs["browse-all"]
        for command, text in outputs.items():
            with self.subTest(command=command):
                self.assertEqual(semantic_errors(command, text.encode(), oracle), [])
                for empty in (b"", b"\x1b[48;2;1;2;3m "):
                    self.assertTrue(semantic_errors(command, empty, oracle))
        for command, changed in [("list", outputs["list"].replace("fixture-003", "fixture-002")),
                                 ("list-v", outputs["list-v"].replace("2023-11-14", "", 1)),
                                 ("list-v", outputs["list-v"].replace("1K", "")),
                                 ("list-v", outputs["list-v"].replace("1K", "2K", 1)),
                                 ("preview", outputs["preview"].replace("fixture-000.png", "fixture-999.png")),
                                 ("preview", outputs["preview"].replace("(1K)", "")),
                                 ("search-metadata", outputs["search-metadata"].replace("fixture-002", "fixture-001")),
                                 ("search-color", outputs["search-color"].replace("4 of 4", "3 of 4")),
                                 ("index", outputs["index"].replace("cached 4", "cached 3")),
                                 ("help", outputs["help"].replace("Library Commands:", ""))]:
            self.assertTrue(semantic_errors(command, changed.encode(), oracle), command)
        self.assertTrue(semantic_errors("list", SGR.sub("", outputs["list"]).encode(), oracle))
        before = palette_content("list", outputs["list"].encode(), 80)
        after = palette_content("list", outputs["list"].replace("48;2;0;0;0m", "48;2;1;2;3m").encode(), 80)
        self.assertEqual(len(changed_palettes(before, after)), 4)
        self.assertFalse(changed_palettes(before, before))
        footer = "\nnewest 10 of 64 — more: theme list -n <count>, or --all"
        for command in ("list", "list-v"):
            text = ("wallpapers\nTITLE COLORSCHEME SOURCE FORMAT SIZE ADDED\n" +
                    rows(range(10), "png 1K 2023-11-14"))
            large = oracle | {"images": 64}
            self.assertEqual(semantic_errors(command, (text + footer).encode(), large), [])
            self.assertTrue(semantic_errors(command, text.encode(), large))
            self.assertTrue(semantic_errors(command, (text + footer.replace("64", "10")).encode(), large))

    def test_width_and_control_detection(self):
        self.assertEqual(cell_width("a界e\u0301"), 4)
        self.assertEqual(rendering(b"\x1b[31mhello\x1b[0m\n", 5)["overflow_lines"], [])
        self.assertEqual(rendering(b"abcdef\n", 5)["overflow_lines"], [{"line": 1, "cells": 6}])
        self.assertIn("U+001B", rendering(b"\x1b[2J", 80)["unexpected_controls"])

    def test_deterministic_png(self):
        data = png(1, 2, 3)
        self.assertEqual(data, png(1, 2, 3))
        self.assertEqual(struct.unpack(">II", data[16:24]), (2, 3))
        self.assertNotEqual(data, png(2, 2, 3))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path)
    parser.add_argument("--after", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--library", type=Path, help="explicit read-only library; defaults to deterministic generated PNGs")
    parser.add_argument("--built-artifacts", action="store_true",
                        help="explicit CI mode for comparable direct native builds with the synthetic fixture; standalone timings remain strict by default")
    parser.add_argument("--images", type=int, default=64)
    parser.add_argument("--commands", nargs="+", help="explicit subset for bounded controls; omitted runs the full CI matrix")
    parser.add_argument("--samples", type=int, default=63, help="warm pairs, at least 21 and divisible by 3")
    parser.add_argument("--cold-samples", type=int, default=9)
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return not unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(SelfTests)).wasSuccessful()
    if not all((args.before, args.after, args.output)):
        parser.error("--before, --after, and --output are required")
    if args.samples < 21 or args.samples % 3 or args.cold_samples < 9 or args.images < 2 or not 0 < args.timeout < math.inf:
        parser.error("require samples >= 21 divisible by 3, cold-samples >= 9, images >= 2, finite timeout > 0")
    try:
        harness = Harness(args)
        comparisons = harness.measure()
        report = harness.report(comparisons)
        print(f"Evidence: {harness.output}")
        return not report["passed"]
    except (OSError, ValueError) as error:
        print(f"performance: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
