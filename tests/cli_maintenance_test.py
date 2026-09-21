"""Narrow displays, duplicate installs, and real shell completion evaluation."""
import argparse
import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location("browser", Path(__file__).with_name("browser_cli_test.py"))
browser = importlib.util.module_from_spec(spec)
spec.loader.exec_module(browser)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--require-all-shells", action="store_true")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="theme-maintenance-") as directory:
        root = Path(directory)
        for name in ("library", "bin", "other", "alias", "config", "tmp"):
            (root / name).mkdir()
        for i in range(3):
            browser.png(root / f"library/中国-é-{i}.png", (30, 70, 120))
        env = os.environ | {"THEME_NO_APPLY": "1", "THEME_NO_UPDATE_CHECK": "1",
            "THEME_WALLPAPER_DIR": str(root / "library"), "THEME_CACHE_DIR": str(root / "cache"),
            "CONFIG_DIR": str(root / "config"), "KITTY_CONFIG_DIRECTORY": str(root / "config"),
            "TMPDIR": str(root / "tmp"), "KITTY_WINDOW_ID": "", "TERM": "dumb",
            "THEME_OPACITY": "1", "THEME_CONTRAST": "4.5", "THEME_FORMATS": "png",
            "THEME_EXCLUDE_FORMATS": ""}

        def run(argv, **kwargs):
            return subprocess.run(argv, env=env, capture_output=True, text=True, timeout=30, **kwargs)

        strip = lambda text: re.sub(r"\x1b\[[0-9;:]*m", "", text).replace("\r", "")
        for width in (12, 25, 40, 44, 60, 80, 87, 120):
            for flags in ([], ["-v"]):
                argv = ["list", "-n", "1", *flags]
                env["COLUMNS"] = str(width)
                done = run([str(binary), *argv])
                assert done.returncode == 0, done.stderr
                lines = strip(done.stdout).splitlines()
                assert all(browser.cells(line) <= width for line in lines), (width, lines)
                assert "newest" in done.stdout and "SOURCE" in done.stdout if flags else "newest" in done.stdout
                if browser.pty_available():
                    env.pop("COLUMNS")
                    with browser.Terminal(binary, env, argv, columns=width) as terminal:
                        terminal.drain(2)
                        assert terminal.process.wait(timeout=5) == 0
                        lines = strip(terminal.output.decode()).splitlines()
                        assert all(browser.cells(line) <= width for line in lines), (width, lines)
                elif os.environ.get("CI"):
                    raise AssertionError("PTY coverage required in CI")

        # A path alias/hardlink is one installation; a different executable is
        # reported without executing potentially hostile PATH bytes.
        (root / "bin/theme").symlink_to(binary)
        (root / "alias/theme").symlink_to(binary)
        other = root / "other/theme"
        other.write_text(f"#!/bin/sh\ntouch '{root}/executed'\n")
        other.chmod(0o700)
        env["PATH"] = os.pathsep.join(map(str, [root / "bin", root / "alias"]))
        assert "multiple installations" not in run([str(binary), "version"]).stderr
        env["PATH"] += os.pathsep + str(root / "other")
        done = run([str(binary), "version"])
        assert "multiple installations" in done.stderr and str(other) in done.stderr
        assert not run([str(binary), "-V"]).stderr
        assert not (root / "executed").exists()
        env["PATH"] = os.environ["PATH"]

        scripts = {}
        for shell in ("bash", "zsh", "fish", "nushell"):
            done = run([str(binary), "completions", shell])
            assert done.returncode == 0 and not done.stderr, done.stderr
            scripts[shell] = root / ("theme." + shell)
            scripts[shell].write_text(done.stdout)
        assert run([str(binary), "completions", "unknown"]).returncode == 1
        assert run([str(binary), "completions", "bash", "extra"]).returncode == 1

        probes = {
            "bash": 'source "$1"; COMP_WORDS=(theme b); COMP_CWORD=1; _theme; printf "%s\\n" "${COMPREPLY[@]}"; COMP_WORDS=(theme update --b); COMP_CWORD=2; _theme; printf "%s\\n" "${COMPREPLY[@]}"',
            "zsh": 'compdef() { :; }; compadd() { shift; print -l -- "$@"; }; source "$1"; words=(theme b); CURRENT=2; _theme; words=(theme update --b); CURRENT=3; _theme',
            "fish": f'source "{scripts["fish"]}"; complete -C "theme b"; complete -C "theme update --b"',
            "nushell": f'source "{scripts["nushell"]}"; print (theme commands | to json); scope commands | where name == "theme update" | get signatures | to json',
        }
        for shell, code in probes.items():
            program = shutil.which("nu" if shell == "nushell" else shell)
            if not program:
                assert not args.require_all_shells, f"missing {shell}"
                print(f"completion {shell}: unavailable locally; required in NixOS CI")
                continue
            argv = [program, "-c", code]
            if shell in ("bash", "zsh"):
                argv = [program, "--norc" if shell == "bash" else "-f", "-c", code, "theme-test", str(scripts[shell])]
            done = run(argv)
            assert done.returncode == 0, (shell, done.stderr)
            assert "browse" in done.stdout and "binary" in done.stdout, (shell, done.stdout)
            if shell == "fish":
                done = run([program, "-c", f'source "{scripts[shell]}"; complete -C "theme update --rot"'])
                assert done.returncode == 0 and not done.stdout, done.stdout
                done = run([program, "-c", f'source "{scripts[shell]}"; complete -C "theme set --rotate r"'])
                assert done.returncode == 0 and "right" in done.stdout, done.stdout
            if shell == "nushell":
                probe = root / "completion-bin"
                probe.mkdir()
                (probe / "theme").write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
                (probe / "theme").chmod(0o700)
                old_path = env["PATH"]
                env["PATH"] = str(probe) + os.pathsep + old_path
                for flag in ("--extend", "--extend=112233"):
                    done = run([program, "-c", f'source "{scripts[shell]}"; theme set wallpaper {flag}'])
                    assert done.returncode == 0 and done.stdout.splitlines() == ["set", "wallpaper", flag], done
                env["PATH"] = old_path
            print(f"completion {shell}: PASS")
    print("maintenance CLI: PASS")


if __name__ == "__main__":
    main()
