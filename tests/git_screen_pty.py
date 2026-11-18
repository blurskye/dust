#!/usr/bin/env python3
"""Exercise standalone and embedded Dusty Git in a real PTY, using disposable Git data."""
import argparse
import pathlib
import subprocess
import tempfile

from pty_smoke import assert_restored, run, terminal_screen


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.STDOUT, text=True).strip()


def fixture(root):
    git(root, "init", "-b", "main")
    git(root, "config", "user.name", "PTY Test")
    git(root, "config", "user.email", "pty@example.invalid")
    (root / ".gitignore").write_text(".dusty-*\n")
    (root / "file.txt").write_text("old\n")
    git(root, "add", ".")
    git(root, "commit", "-m", "Initial")
    (root / "file.txt").write_text("actual terminal input\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--standalone", required=True, type=pathlib.Path)
    parser.add_argument("--editor", required=True, type=pathlib.Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        fixture(root)
        code, output, snapshots = run(args.standalone.resolve(), tmp, [], [
            (3, b" ", None),
            (2, b"c", None),
            (0.3, b"Native PTY commit", None),
            (0.3, b"\r\r", None),
            (2, b"2L", None),
            (0.5, b"q", None),
        ], seen_workspace=False, stage_expectations={0: lambda s: "actual terminal input" in s})
        assert code == 0, terminal_screen(output)
        assert git(root, "log", "-1", "--format=%s") == "Native PTY commit"
        assert "Worktrees" in terminal_screen(snapshots[-1])
        assert b"\x1b[6 q" in output or b"\x1b[5 q" in output, "missing native text caret"
        assert_restored(output)
        print("PASS standalone: real keyboard staging, typed commit, worktree subview, cursor and terminal cleanup")
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        fixture(root)
        code, output, snapshots = run(args.editor.resolve(), tmp, ["--safe-mode", "file.txt"], [
            (3, b":workspace.git.open\r", None),
            (3, b"2L", None),
            (0.5, b"q", None),
            (0.3, b":qa!\r", None),
        ], seen_workspace=False, stage_expectations={1: lambda s: "Dusty Git" in s})
        assert code == 0, terminal_screen(output)
        assert "Dusty Git" in terminal_screen(snapshots[1])
        assert "Worktrees" in terminal_screen(snapshots[2])
        assert git(root, "log", "-1", "--format=%s") == "Initial"
        assert_restored(output)
        print("PASS embedded: shared native screen, pane keys, return to editor, terminal cleanup")
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        fixture(root)
        code, output, snapshots = run(args.editor.resolve(), tmp, ["--safe-mode", "file.txt"], [
            (3, b":workspace.git.open\r", None),
            (3, b"\r", None),
            (1, b":workspace.git.open\r", None),
            (3, b":tutor git\r", None),
            (3, b"345", None),
            (2, b"2", None),
            (1, b":tutor quit\r", None),
            (1, b"q", None),
            (0.3, b":qa!\r", None),
        ], seen_workspace=False, stage_expectations={1: lambda s: "Dusty Git" in s, 3: lambda s: "Dusty Git" in s, 4: lambda s: "Git tutor · 1/48" in s, 5: lambda s: "Git tutor · 1/48" in s, 6: lambda s: "Git tutor · 2/48" in s})
        assert code == 0, terminal_screen(output)
        floating = terminal_screen(snapshots[1]).splitlines()
        position = next((y, line.index("Dusty Git")) for y, line in enumerate(floating) if "Dusty Git" in line)
        assert position[0] > 0 and position[1] > 1, "Git is not floating over the editor"
        assert "Dusty Git" not in terminal_screen(snapshots[2])
        assert "Dusty Git" in terminal_screen(snapshots[3])
        assert "Git tutor · 1/48" in terminal_screen(snapshots[4])
        assert "Git tutor · 1/48" in terminal_screen(snapshots[5]), "lesson 1 finishes only back on Changes"
        assert "Git tutor · 2/48" in terminal_screen(snapshots[6])
        assert "Git tutor" not in terminal_screen(snapshots[7])
        assert (root / "file.txt").read_text() == "actual terminal input\n"
        assert git(root, "log", "-1", "--format=%s") == "Initial"
        assert_restored(output)
        print("PASS embedded: floating geometry, file-open/resume, real tutor commands and original repository restoration")
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        fixture(root)
        config = root / ".dusty-config"
        config.mkdir()
        (config / "config.toml").write_text('[ui.git]\nplacement = "fullscreen"\n')
        code, output, snapshots = run(args.standalone.resolve(), tmp, [], [(3, b"q", None)], clean=False, config_home=config, seen_workspace=False)
        assert code == 0, terminal_screen(output)
        assert "Dusty Git" in terminal_screen(snapshots[0]).splitlines()[0]
        assert_restored(output)
        print("PASS standalone: saved fullscreen setting is honored")


if __name__ == "__main__":
    main()
