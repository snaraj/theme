"""Require the real terminal oracle to catch dropped image and color delivery."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--kitty", required=True)
parser.add_argument("--theme", required=True)
parser.add_argument("--output", required=True, type=Path)
args = parser.parse_args()
for mutation, evidence in [("drop-images", "sheet paints the named pictures at their source aspect ratios"),
                           ("drop-colors", "first changes every existing terminal color")]:
    directory = args.output / mutation
    result = subprocess.run([sys.executable, "-I", "-B", str(Path(__file__).with_name("kitty_e2e.py")),
        "--kitty", args.kitty, "--theme", args.theme, "--output", str(directory), "--mutation", mutation],
        timeout=300)
    summary = json.loads((directory / "summary.json").read_text())
    checks = {entry["name"]: entry["ok"] for entry in summary["assertions"]}
    assert result.returncode == 1 and checks.get(evidence) is False, (mutation, checks)
    assert checks.get("kitty runs the browser") is True and "failure" not in summary, summary
    if mutation == "drop-colors":
        assert checks.get("sheet paints the named pictures at their source aspect ratios") is True
        assert checks.get("first regenerates all ANSI colors from the selected image") is True
    print(f"Kitty mutation {mutation}: caught by {evidence}", flush=True)
