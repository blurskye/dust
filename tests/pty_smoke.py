#!/usr/bin/env python3
"""Linux/macOS PTY smoke tests for Dust's real terminal lifecycle and editing."""

from __future__ import annotations

import argparse
import fcntl
import os
import pathlib
import pty
import re
import select
import shutil
import struct
import tempfile
import termios
import time
import unicodedata
from collections.abc import Callable

Stage = tuple[float, bytes, tuple[int, int] | None]


def set_size(descriptor: int, rows: int, columns: int) -> None:
    fcntl.ioctl(descriptor, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def run(
    binary: pathlib.Path,
    cwd: str,
    arguments: list[str],
    stages: list[Stage],
    *,
    clean: bool = True,
    config_home: pathlib.Path | None = None,
    stage_actions: dict[int, Callable[[], None]] | None = None,
) -> tuple[int, bytes, list[bytes]]:
    pid, descriptor = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.environ["TERM"] = "xterm-256color"
        os.environ["DUST_CONFIG_HOME"] = str(
            config_home or pathlib.Path(cwd) / ".dust-missing-config"
        )
        os.environ["DUST_STATE_HOME"] = str(pathlib.Path(cwd) / ".dust-state")
        command = [str(binary)]
        if clean:
            command.append("--clean")
        os.execv(str(binary), [*command, *arguments])

    set_size(descriptor, 30, 100)
    output = b""
    snapshots: list[bytes] = []
    for stage_index, (delay, data, size) in enumerate(stages):
        deadline = time.time() + delay
        while time.time() < deadline:
            readable, _, _ = select.select([descriptor], [], [], 0.02)
            if readable:
                try:
                    output += os.read(descriptor, 65536)
                except OSError:
                    break
        snapshots.append(output)
        if stage_actions and stage_index in stage_actions:
            stage_actions[stage_index]()
        if size is not None:
            set_size(descriptor, *size)
        if data:
            os.write(descriptor, data)

    deadline = time.time() + 5
    status = None
    while time.time() < deadline:
        readable, _, _ = select.select([descriptor], [], [], 0.05)
        if readable:
            try:
                output += os.read(descriptor, 65536)
            except OSError:
                pass
        exited, observed_status = os.waitpid(pid, os.WNOHANG)
        if exited:
            status = observed_status
            break
    if status is None:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
        raise AssertionError("Dust did not exit within five seconds")
    return os.waitstatus_to_exitcode(status), output, snapshots


def terminal_projection(
    data: bytes, width: int = 100, height: int = 30
) -> tuple[str, tuple[int, int]]:
    source = data.decode("utf-8", "replace")
    grid = [[" "] * width for _ in range(height)]
    x = 0
    y = 0
    index = 0
    while index < len(source):
        if source[index] == "\x1b":
            if index + 1 < len(source) and source[index + 1] == "[":
                match = re.match(r"\x1b\[([?0-9;:]*)([ -/]*)([@-~])", source[index:])
                if match:
                    raw_parameters = match.group(1).lstrip("?")
                    parameters = raw_parameters.split(";") if raw_parameters else []
                    numbers = [int(parameter or 0) for parameter in parameters]
                    final = match.group(3)
                    if final in ("H", "f"):
                        y = (numbers[0] if numbers else 1) - 1
                        x = (numbers[1] if len(numbers) > 1 else 1) - 1
                    elif final == "G":
                        x = (numbers[0] if numbers else 1) - 1
                    elif final == "d":
                        y = (numbers[0] if numbers else 1) - 1
                    elif final == "J" and numbers and numbers[0] in (2, 3):
                        grid = [[" "] * width for _ in range(height)]
                    index += match.end()
                    continue
            index += 2
            continue
        character = source[index]
        index += 1
        if character == "\r":
            x = 0
            continue
        if character == "\n":
            y += 1
            continue
        if character < " ":
            continue
        if 0 <= x < width and 0 <= y < height:
            grid[y][x] = character
        x += 2 if unicodedata.east_asian_width(character) in "WF" else 1
    return "\n".join("".join(row) for row in grid), (x, y)


def terminal_screen(data: bytes, width: int = 100, height: int = 30) -> str:
    return terminal_projection(data, width, height)[0]


def terminal_cursor(data: bytes, width: int = 100, height: int = 30) -> tuple[int, int]:
    return terminal_projection(data, width, height)[1]


def assert_restored(output: bytes) -> None:
    decoded = output.decode("utf-8", "ignore")
    assert "\x1b[?1049h" in decoded
    assert "\x1b[?1049l" in decoded
    assert "\x1b[?2004h" in decoded
    assert "\x1b[?2004l" in decoded


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, default=pathlib.Path("target/debug/dust"))
    arguments = parser.parse_args()
    binary = arguments.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"Dust binary does not exist: {binary}; run `cargo build`")

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.25, b":", None), (0.25, b"q\r", None)],
        )
        assert code == 0, code
        assert pathlib.Path(directory).as_posix() in terminal_screen(snapshots[0])
        assert "Command" in terminal_screen(snapshots[1])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.2, b":lsp\r", None),
                (0.3, b"", None),
                (0.1, b"\x1b", None),
                (0.1, b":q\r", None),
            ],
        )
        assert code == 0
        tool_screen = terminal_screen(snapshots[1])
        assert "Dust tool manager" in tool_screen
        assert "rust-analyzer" in tool_screen
        assert "LSP" in tool_screen and ("Available" in tool_screen or "Installed" in tool_screen)
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.2, b":commands\r", None),
                (0.2, b"health", None),
                (0.2, b"\x1b", None),
                (0.1, b":q\r", None),
            ],
        )
        assert code == 0
        picker_screen = terminal_screen(snapshots[2])
        assert "Commands" in picker_screen
        assert "health.open" in picker_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        first = root / "alpha.txt"
        second = root / "beta.txt"
        first.write_text("needle alpha\n", encoding="utf-8")
        second.write_text("beta needle\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.2, b":grep needle\r", None),
                (0.6, b"alpha", None),
                (0.2, b"\x1b", None),
                (0.1, b":workspace.replace needle changed\r", None),
                (0.6, b"\x1b", None),
                (0.2, b":q\r", None),
            ],
        )
        assert code == 0
        search_screen = terminal_screen(snapshots[1])
        filtered_screen = terminal_screen(snapshots[2])
        preview_screen = terminal_screen(snapshots[4])
        assert "Workspace search" in search_screen and "alpha.txt" in search_screen
        assert "alpha.txt" in filtered_screen and "beta.txt" not in filtered_screen
        assert "Workspace replacement" in preview_screen
        assert "alpha.txt" in preview_screen and "beta.txt" in preview_screen
        assert first.read_text(encoding="utf-8") == "needle alpha\n"
        assert second.read_text(encoding="utf-8") == "beta needle\n"
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.4, b":workspace.trust.status\r", None),
                (0.2, b":workspace.trust\r", None),
                (0.2, b"wrong\r", None),
                (0.3, b":workspace.trust\r", None),
                (0.2, b"trust this workspace\r", None),
                (0.5, b":q\r", None),
            ],
        )
        assert code == 0
        assert "untrusted" in terminal_screen(snapshots[1])
        assert "Confirm workspace trust" in terminal_screen(snapshots[2])
        assert "did not match" in terminal_screen(snapshots[3])
        assert "Confirm workspace trust" in terminal_screen(snapshots[4])
        assert "workspace trusted" in terminal_screen(snapshots[5])
        assert_restored(output)

        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.4, b":workspace.trust.status\r", None),
                (0.2, b":q\r", None),
            ],
        )
        assert code == 0
        assert "workspace is trusted" in terminal_screen(snapshots[1])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root_b = pathlib.Path(directory) / "root-b"
        root_b.mkdir()
        target = root_b / "from-b.txt"
        target.write_text("second root\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.4, f":rootadd {root_b}\r".encode(), None),
                (0.5, b":roots\r", None),
                (0.2, b"root-b", None),
                (0.2, b"\r", None),
                (0.2, b":rootstatus\r", None),
                (0.2, f":e {target}\r".encode(), None),
                (0.8, b":q\r", None),
            ],
        )
        assert code == 0
        assert "added workspace root" in terminal_screen(snapshots[1])
        assert "Workspace roots" in terminal_screen(snapshots[2])
        assert "root-b" in terminal_screen(snapshots[3])
        assert "active root" in terminal_screen(snapshots[5])
        opened = terminal_screen(snapshots[6])
        assert "second root" in opened and "from-b.txt" in opened
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        nested = pathlib.Path(directory) / "nested"
        nested.mkdir()
        target = nested / "inside.txt"
        target.write_text("explorer pty content\n", encoding="utf-8")
        code, output, snapshots = run(
            binary, directory, [],
            [(0.7,b":explore\r",None),(0.2,b"l",None),(0.7,b"j",None),(0.2,b"\r",None),(0.8,b":q\r",None)],
        )
        assert code == 0
        assert "Explorer" in terminal_screen(snapshots[1])
        assert "nested" in terminal_screen(snapshots[1])
        assert "inside.txt" in terminal_screen(snapshots[3])
        opened_with_dock = terminal_screen(snapshots[4])
        assert "Explorer" in opened_with_dock
        assert "inside.txt" in opened_with_dock
        assert "explorer pty content" in opened_with_dock
        assert opened_with_dock.splitlines()[0][40] == "⎸"
        assert_restored(output)

        code, output, snapshots = run(
            binary, directory, [],
            [(0.7,b":explore\r",None),(0.2,b"l",None),(0.7,b"j",None),(0.2,b"s",None),(0.8,b":q!\r",None)],
        )
        assert code == 0
        split_screen = terminal_screen(snapshots[4])
        assert "Explorer" in split_screen and "inside.txt" in split_screen
        assert "explorer pty content" in split_screen
        assert split_screen.splitlines()[0][40] == "⎸"
        assert_restored(output)

    # Directional focus crosses the persistent explorer/editor boundary using
    # the terminal's real Ctrl-h/Ctrl-l bytes, not synthetic command calls.
    with tempfile.TemporaryDirectory() as directory:
        target = pathlib.Path(directory) / "focus.txt"
        target.write_text("directional focus\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(target)],
            [
                (0.7, b":explore\r", None),
                (0.7, b"", None),
                (0.1, b"\x0c", None),  # Ctrl-l: explorer -> editor
                (0.3, b"", None),
                (0.1, b"\x08", None),  # Ctrl-h: leftmost editor -> explorer
                (0.3, b"", None),
                (0.1, b"\x1b", None),
                (0.2, b":q\r", None),
            ],
        )
        assert code == 0
        focused_explorer = terminal_screen(snapshots[1]).splitlines()[1]
        focused_editor = terminal_screen(snapshots[3]).splitlines()[1]
        refocused_explorer = terminal_screen(snapshots[5]).splitlines()[1]
        assert focused_explorer.lstrip().startswith(">")
        assert not focused_editor.lstrip().startswith(">")
        assert refocused_explorer.lstrip().startswith(">")
        assert_restored(output)

    # A real terminal cursor follows a long non-wrapped line through narrow
    # resize and explorer-dock geometry, then remains in the editor after the
    # dock is widened again.
    with tempfile.TemporaryDirectory() as directory:
        target = pathlib.Path(directory) / "horizontal.txt"
        target.write_text("0123456789" * 24 + "\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(target)],
            [
                (0.5, b"$", None),
                (0.3, b"", (8, 20)),
                (0.5, b":explore\r", None),
                (0.8, b"", None),
                (0.1, b"\x0c", None),  # Ctrl-l: explorer -> editor
                (0.4, b"", (12, 60)),
                (0.5, b":q\r", None),
            ],
        )
        assert code == 0
        initial_cursor = terminal_cursor(snapshots[1], 100, 30)
        narrow_cursor = terminal_cursor(snapshots[2], 20, 8)
        explorer_cursor = terminal_cursor(snapshots[3], 20, 8)
        narrow_editor_cursor = terminal_cursor(snapshots[5], 20, 8)
        wide_editor_cursor = terminal_cursor(snapshots[6], 60, 12)
        assert 0 <= initial_cursor[0] < 100 and 0 <= initial_cursor[1] < 29
        assert 0 <= narrow_cursor[0] < 20 and 0 <= narrow_cursor[1] < 7
        assert explorer_cursor[0] < 9
        assert 10 <= narrow_editor_cursor[0] < 20 and narrow_editor_cursor[1] < 7
        assert 40 <= wide_editor_cursor[0] < 60 and wide_editor_cursor[1] < 11
        assert_restored(output)

    # Open text buffers have a real editor-region content line after the
    # explorer. Physical Shift-h/l (logical H/L) navigate independently from
    # logical workspace tabs.
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        first = root / "alpha.rs"
        second = root / "beta.rs"
        third = root / "gamma.rs"
        first.write_text("ALPHA-CONTENT\n", encoding="utf-8")
        second.write_text("BETA-CONTENT\n", encoding="utf-8")
        third.write_text("GAMMA-CONTENT\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(first)],
            [
                (0.5, f":edit {second}\r".encode(), None),
                (0.6, f":edit {third}\r".encode(), None),
                (0.6, b":explore\r", None),
                (0.4, b"\x0c", None),  # explorer -> editor
                (0.3, b"H", None),
                (0.3, b"L", None),
                (0.3, b":q\r", None),
            ],
        )
        assert code == 0
        opened = terminal_screen(snapshots[3])
        top = opened.splitlines()[0]
        assert "Explorer" in top[:41]
        assert all(name in top[41:] for name in ("alpha.rs", "beta.rs", "gamma.rs"))
        assert "BETA-CONTENT" in terminal_screen(snapshots[5])
        assert "GAMMA-CONTENT" in terminal_screen(snapshots[6])
        assert_restored(output)

    # A real shell is rendered as a persistent full-width bottom dock. The
    # editor/explorer remain above it, both default Ctrl+/ and Ctrl+\\ toggles
    # work through legacy control-byte normalization without hiding it, and
    # exact resize/refocus/exit paths preserve terminal restoration.
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        target = root / "terminal-dock.txt"
        target.write_text("EDITOR-TOP\n", encoding="utf-8")
        config_home = root / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            "[ui.terminal_panel]\nheight=10\n",
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            [str(target)],
            [
                (0.4, b":trust\r", None),
                (0.2, b"trust this workspace\r", None),
                (0.5, b":explore\r", None),
                (0.7, b"\x0c", None),  # explorer -> editor
                (0.7, b"\x1f", None),  # legacy Ctrl+/ opens bundled shell
                (0.3, b"stty -echo\r", None),
                (
                    1.0,
                    b"printf '%b' '\\033\\133?2004h\\033\\1331;32mDOCKED-TERMINAL\\033\\1330m\\033\\1350;title\\a\\n'\r",
                    None,
                ),
                (1.5, b"\x1f", None),  # allow bounded PTY output, then return focus
                (0.3, b"", (20, 60)),
                (0.7, b"\x1c", None),  # legacy Ctrl+\\ focuses the same PTY
                (0.4, b"exit\r", None),
                (0.6, b"\x1b", None),
                (0.2, b":q\r", None),
            ],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        focused = terminal_screen(snapshots[7], 100, 30)
        focused_rows = focused.splitlines()
        assert "Explorer" in focused and "EDITOR-TOP" in focused
        assert "TERMINAL #" in focused_rows[20]
        assert any("DOCKED-TERMINAL" in row for row in focused_rows[21:29])
        assert "[?2004h" not in focused and "[1;32m" not in focused
        assert "]0;title" not in focused
        assert "TERMINAL" in focused_rows[29] and "RUNNING" in focused_rows[29]
        assert focused_rows[21][40] != "⎸"  # terminal spans beneath explorer

        unfocused = terminal_screen(snapshots[8], 100, 30)
        assert "TERMINAL #" in unfocused.splitlines()[20]
        assert "EDITOR" in unfocused.splitlines()[29]
        assert terminal_cursor(snapshots[8], 100, 30)[1] < 20

        resized = terminal_screen(snapshots[9], 60, 20)
        assert "TERMINAL #" in resized.splitlines()[10]
        assert "EDITOR" in resized.splitlines()[19]
        refocused = terminal_screen(snapshots[10], 60, 20)
        assert "TERMINAL" in refocused.splitlines()[19]
        assert terminal_cursor(snapshots[10], 60, 20)[1] >= 11
        exited = terminal_screen(snapshots[11], 60, 20)
        assert "EXIT 0" in exited.splitlines()[19]
        assert_restored(output)

    if shutil.which("rust-analyzer"):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "main.rs"
            path.write_text("fn main() {}\n", encoding="utf-8")
            code, output, snapshots = run(
                binary,
                directory,
                [str(path)],
                [(0.4,b":workspace.trust\r",None),(0.2,b"trust this workspace\r",None),(0.5,b":lsp.server.start\r",None),(1.2,b":lsp.server.status\r",None),(0.2,b":q\r",None)],
            )
            assert code == 0
            assert "rust-analyzer" in terminal_screen(snapshots[4])
            assert_restored(output)

    for executable, suffix, source, server in [
        ("typescript-language-server", ".ts", "const value: number = 1;\n", "typescript-language-server"),
        ("gopls", ".go", "package main\nfunc main() {}\n", "gopls"),
    ]:
        if not shutil.which(executable):
            continue
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / f"main{suffix}"
            path.write_text(source, encoding="utf-8")
            code, output, snapshots = run(
                binary,
                directory,
                [str(path)],
                [(0.4,b":workspace.trust\r",None),(0.2,b"trust this workspace\r",None),(0.5,b":lsp.server.start\r",None),(1.2,b":lsp.server.status\r",None),(0.2,b":q\r",None)],
            )
            assert code == 0
            assert server in terminal_screen(snapshots[4])
            assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(binary, directory, ["."], [(0.2, b":q\r", None)])
        assert code == 0
        assert pathlib.Path(directory).as_posix() in terminal_screen(snapshots[0])
        assert_restored(output)

        child = pathlib.Path(directory) / "workspace"
        child.mkdir()
        code, output, snapshots = run(
            binary,
            directory,
            [str(child)],
            [(0.2, b":q\r", None)],
        )
        assert code == 0
        assert child.as_posix() in terminal_screen(snapshots[0])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        config_home = pathlib.Path(directory) / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            "[startup]\nshow_dashboard=false\n",
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.2, b":q\r", None)],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        startup_screen = terminal_screen(snapshots[0])
        assert "D U S T" not in startup_screen and "[scratch]" in startup_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        config_home = pathlib.Path(directory) / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            "[ui]\nalternate_screen=false\nmouse=false\nfocus_events=false\n",
            encoding="utf-8",
        )
        code, output, _ = run(
            binary,
            directory,
            [],
            [(0.2, b"q", None)],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        assert b"\x1b[?1049h" not in output
        assert b"\x1b[?1000h" not in output
        assert b"\x1b[?2004h" in output and b"\x1b[?2004l" in output

    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "focus.txt"
        path.write_text("one two one\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [
                (0.2, b"/", None),
                (0.2, b"\x1b", None),
                (0.1, b"?", None),
                (0.2, b"\x1b", None),
                (0.1, b":q\r", None),
            ],
        )
        assert code == 0
        initial = terminal_screen(snapshots[0])
        assert "focus.txt" in initial and "NORMAL" in initial and "D U S T" not in initial
        assert "Search Forward" in terminal_screen(snapshots[1])
        assert "Search Backward" in terminal_screen(snapshots[3])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "modes.txt"
        path.write_text("abc\ndef\nghi\njkl\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [
                (0.2, b"RXY\x7f", None),
                (0.1, b"\x1b", None),
                (0.1, b"\x16jlcZ", None),
                (0.1, b"\x1b", None),
                (0.1, b"jj.", None),
                (0.2, b":q!\r", None),
            ],
        )
        assert code == 0
        mode_screen = terminal_screen(snapshots[5])
        assert all(fragment in mode_screen for fragment in ("Zc", "Zf", "Zi", "Zl"))
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "recover-me.txt"
        path.write_text("base\n", encoding="utf-8")
        code, output, _ = run(
            binary,
            directory,
            [str(path)],
            [(0.2, b"iunsaved-\x1b", None), (0.3, b":q!\r", None)],
        )
        assert code == 0
        assert_restored(output)
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.4, b":recovery\r", None),
                (0.2, b":recover\r", None),
                (0.2, b":q\r", None),
            ],
        )
        assert code == 0
        assert "1 recovery journal" in terminal_screen(snapshots[1])
        assert "unsaved-base" in terminal_screen(snapshots[2])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "layout.txt"
        path.write_text("one\ntwo\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [
                (0.2, b"\x17s", None),
                (0.2, b":tabnew\r", None),
                (0.2, b":q\r", None),
            ],
        )
        assert code == 0
        assert "─" in terminal_screen(snapshots[1])
        assert "[layout.txt]" in terminal_screen(snapshots[2])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.2, b":", None),
                (0.2, b"", (15, 50)),
                (0.2, b"q\r", None),
            ],
        )
        assert code == 0
        assert "Command" in terminal_screen(snapshots[1])
        assert "Command" in terminal_screen(snapshots[2])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.2, b":health\r", None), (0.2, b":q\r", None)],
        )
        assert code == 0
        assert "Dust health" in terminal_screen(snapshots[1])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        config_home = pathlib.Path(directory) / "config"
        config_home.mkdir()
        (config_home / "keymaps.toml").write_text(
            """
[[keymap]]
id = "builtin.dashboard.surface.editor-quit.q"
context = "dashboard"
keys = "z"
command = "editor.quit"
arguments = { force = false }
""".strip(),
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.2, b"q", None), (0.2, b"z", None)],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        remapped = terminal_screen(snapshots[1])
        assert pathlib.Path(directory).as_posix() in remapped
        assert "[z]" in remapped and "[q]" not in remapped
        assert_restored(output)

        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.2, b"q", None)],
            clean=True,
            config_home=config_home,
        )
        assert code == 0
        clean_screen = terminal_screen(snapshots[0])
        assert "[q]" in clean_screen and "[z]" not in clean_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        config_home = pathlib.Path(directory) / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            """
[input.keymaps]
load_bundled_defaults = false
leader = ","
local_leader = ";"
""".strip(),
            encoding="utf-8",
        )
        (config_home / "keymaps.toml").write_text(
            """
[[keymap]]
id = "user.dashboard.symbolic-quit"
context = "dashboard"
keys = "<leader> q"
command = "editor.quit"
arguments = { force = false }
""".strip(),
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.2, b"q", None), (0.2, b",q", None)],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        leader_screen = terminal_screen(snapshots[0])
        assert "[, q]" in leader_screen and "[q]" not in leader_screen
        assert pathlib.Path(directory).as_posix() in terminal_screen(snapshots[1])
        assert_restored(output)

        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.2, b"q", None)],
            clean=True,
            config_home=config_home,
        )
        assert code == 0
        clean_leader_screen = terminal_screen(snapshots[0])
        assert "[q]" in clean_leader_screen and "[, q]" not in clean_leader_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        path = root / "hints.txt"
        path.write_text("key hint smoke\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [(0.5, b" ", None), (0.45, b"\x1b", None), (0.1, b":q\r", None)],
            clean=True,
        )
        assert code == 0
        hint_screen = terminal_screen(snapshots[1])
        assert "Files" in hint_screen
        assert "Diagnostics" in hint_screen
        assert "Language" in hint_screen
        assert "space" in hint_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        path = root / "custom-hints.txt"
        path.write_text("custom key hint smoke\n", encoding="utf-8")
        config_home = root / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            """
[ui.key_hints]
delay_ms = 50

[input.keymaps]
leader = ","
""".strip(),
            encoding="utf-8",
        )
        (config_home / "keymaps.toml").write_text(
            """
[[key_group]]
id = "builtin.editor.normal.group.files"
context = "editor"
mode = "normal"
prefix = "<leader> f"
label = "Project files"
icon = "F"
order = 1
""".strip(),
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [(0.5, b",", None), (0.2, b"\x1b", None), (0.1, b":q\r", None)],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        custom_hint_screen = terminal_screen(snapshots[1])
        assert "Project files" in custom_hint_screen
        assert "Files" not in custom_hint_screen.replace("Project files", "")
        assert "," in custom_hint_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        path = root / "disabled-hints.txt"
        path.write_text("disabled hint smoke\n", encoding="utf-8")
        config_home = root / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            "[ui.key_hints]\nenabled = false\ndelay_ms = 0\n",
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [(0.5, b" ", None), (0.2, b"\x1b", None), (0.1, b":q\r", None)],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        disabled_hint_screen = terminal_screen(snapshots[1])
        assert "Files" not in disabled_hint_screen
        assert "Diagnostics" not in disabled_hint_screen
        assert "Language" not in disabled_hint_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        config_home = root / "config"
        (config_home / "themes").mkdir(parents=True)
        config_path = config_home / "config.toml"
        config_path.write_text(
            """
[ui]
theme = "custom"

[ui.command_line.text]
command_title = "OPS CONSOLE"
command_marker = "> "

[ui.status_line.mode_labels]
normal = "NORMX"

[input]
disabled_command_aliases = ["q"]

[input.command_aliases]
bye = "editor.quit"
""".strip(),
            encoding="utf-8",
        )
        (config_home / "dashboard.toml").write_text(
            """
show_identity = true
show_version = true
show_path = true
identity = "CUSTOM DASH"
version_format = "release {version}"
resize_message = "MAKE ROOM"
unbound_label = "none"
key_format = "<{key}>"
content_width = 66
minimum_width = 20
minimum_height = 7
block_height = 9
identity_row = 0
version_row = 1
path_row = 3
actions_row = 5
action_key_width = 12
action_left_offset = 2
compact_left = 1
compact_top = 1

[[action]]
command = "command_line.open"
label = "Console"
arguments = { kind = "command" }

[[action]]
command = "editor.quit"
label = "Exit"
arguments = { force = false }
""".strip(),
            encoding="utf-8",
        )
        (config_home / "keymaps.toml").write_text(
            """
[[keymap]]
id = "user.dashboard.console"
context = "dashboard"
keys = "p"
command = "command_line.open"
arguments = { kind = "command" }

[[keymap]]
id = "builtin.dashboard.surface.editor-quit.q"
context = "dashboard"
keys = "z"
command = "editor.quit"
arguments = { force = false }

[[keymap]]
id = "user.editor.normal.quit-z"
context = "editor"
mode = "normal"
keys = "z"
command = "editor.quit"
arguments = { force = false }
""".strip(),
            encoding="utf-8",
        )
        repository = pathlib.Path(__file__).resolve().parents[1]
        custom_theme = (repository / "assets/themes/dust.toml").read_text(encoding="utf-8")
        custom_theme = custom_theme.replace('name = "dust"', 'name = "custom"')
        custom_theme = custom_theme.replace(
            '[styles.accent]\nforeground = "#58a6ff"\nbackground = "#0d1117"\nforeground_256 = "indexed:75"',
            '[styles.accent]\nforeground = "indexed:201"\nbackground = "#0d1117"\nforeground_256 = "indexed:201"',
        )
        (config_home / "themes/custom.toml").write_text(custom_theme, encoding="utf-8")

        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.3, b"p", None),
                (0.2, b"\x1b", None),
                (0.1, b":config reload\r", None),
                (0.5, b":config reload\r", None),
                (1.0, b"p", None),
                (0.2, b"\x1b", None),
                (0.2, b":bye\r", None),
            ],
            clean=False,
            config_home=config_home,
            stage_actions={3: lambda: config_path.write_text("[ui\n", encoding="utf-8")},
        )
        assert code == 0
        assert "CUSTOM DASH" in terminal_screen(snapshots[0])
        assert "OPS CONSOLE" in terminal_screen(snapshots[1])
        assert "configuration reloaded" in terminal_screen(snapshots[3])
        assert "reload rejected" in terminal_screen(snapshots[4])
        assert "OPS CONSOLE" in terminal_screen(snapshots[5])
        assert b"38;5;201" in output
        assert_restored(output)

        config_path.write_text(
            "[ui]\ntheme='custom'\n[ui.status_line.mode_labels]\nnormal='NORMX'\n",
            encoding="utf-8",
        )
        path = root / "configured-status.txt"
        path.write_text("status\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [(0.25, b"z", None)],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        assert "NORMX" in terminal_screen(snapshots[0])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        config_home = root / "config"
        config_home.mkdir()
        config_path = config_home / "config.toml"

        themes = config_home / "themes"
        themes.mkdir()
        repository = pathlib.Path(__file__).resolve().parents[1]
        watched_theme = (repository / "assets/themes/dust.toml").read_text(encoding="utf-8")
        watched_theme = watched_theme.replace('name = "dust"', 'name = "watched"')
        watched_theme = watched_theme.replace(
            '[styles.accent]\nforeground = "#58a6ff"\nbackground = "#0d1117"\nforeground_256 = "indexed:75"',
            '[styles.accent]\nforeground = "indexed:202"\nbackground = "#0d1117"\nforeground_256 = "indexed:202"',
        )
        (themes / "watched.toml").write_text(watched_theme, encoding="utf-8")
        (config_home / "keymaps.toml").write_text(
            "[[keymap]]\nid='user.watch.editor-quit'\ncontext='editor'\nmode='normal'\nkeys='z'\ncommand='editor.quit'\narguments={force=false}\n"
            "[[keymap]]\nid='user.watch.dashboard-quit'\ncontext='dashboard'\nkeys='z'\ncommand='editor.quit'\narguments={force=false}\n",
            encoding="utf-8",
        )

        def watched_config(label: str, theme: str = "watched") -> str:
            return (
                "[config_watch]\n"
                "enabled=true\n"
                "poll_interval_ms=25\n"
                "debounce_ms=25\n"
                "[ui]\n"
                f"theme='{theme}'\n"
                "[ui.status_line.mode_labels]\n"
                f"normal='{label}'\n"
            )

        config_path.write_text(watched_config("WATCH-OLD"), encoding="utf-8")

        watched_dashboard = (
            repository / "assets/defaults/dashboard.toml"
        ).read_text(encoding="utf-8").replace("D U S T", "WATCHED DASHBOARD")

        def update_watched_sources() -> None:
            config_path.write_text(watched_config("WATCH-NEW"), encoding="utf-8")
            (themes / "watched.toml").write_text(
                watched_theme.replace("indexed:202", "indexed:203"), encoding="utf-8"
            )
            (config_home / "dashboard.toml").write_text(
                watched_dashboard, encoding="utf-8"
            )
            (config_home / "keymaps.toml").write_text(
                "[[keymap]]\nid='user.watch.editor-quit'\ncontext='editor'\nmode='normal'\nkeys='y'\ncommand='editor.quit'\narguments={force=false}\n"
                "[[keymap]]\nid='user.watch.dashboard-quit'\ncontext='dashboard'\nkeys='y'\ncommand='editor.quit'\narguments={force=false}\n",
                encoding="utf-8",
            )

        def switch_watched_theme_source() -> None:
            (themes / "watched2.toml").write_text(
                watched_theme.replace('name = "watched"', 'name = "watched2"')
                .replace("indexed:202", "indexed:204"),
                encoding="utf-8",
            )
            config_path.write_text(
                watched_config("WATCH-THEME2", "watched2"), encoding="utf-8"
            )

        def update_selected_theme_content() -> None:
            path = themes / "watched2.toml"
            path.write_text(
                path.read_text(encoding="utf-8").replace("indexed:204", "indexed:205"),
                encoding="utf-8",
            )

        path = root / "watched.txt"
        path.write_text("watch\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [
                (0.25, b"", None),
                (0.4, b"", None),
                (0.4, b"", None),
                (0.8, b"", None),
                (0.8, b":bd\r", None),
                (0.2, b"y", None),
            ],
            clean=False,
            config_home=config_home,
            stage_actions={
                0: update_watched_sources,
                1: switch_watched_theme_source,
                2: update_selected_theme_content,
                3: lambda: config_path.write_text("[ui\n", encoding="utf-8"),
            },
        )
        assert code == 0
        assert "WATCH-OLD" in terminal_screen(snapshots[0])
        assert "WATCH-NEW" in terminal_screen(snapshots[1])
        assert "WATCH-THEME2" in terminal_screen(snapshots[2])
        retained = terminal_screen(snapshots[4])
        assert "WATCH-THEME2" in retained and "automatic configuration reload rejected" in retained
        assert "WATCHED DASHBOARD" in terminal_screen(snapshots[5])
        assert b"38;5;203" in output
        assert b"38;5;205" in output
        assert_restored(output)

        config_path.write_text(watched_config("IGNORED"), encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [(0.2, b"", None), (0.3, b":q\r", None)],
            clean=True,
            config_home=config_home,
            stage_actions={
                0: lambda: config_path.write_text(
                    watched_config("STILL-IGNORED"), encoding="utf-8"
                )
            },
        )
        assert code == 0
        clean_screen = terminal_screen(snapshots[1])
        assert "NORMAL" in clean_screen
        assert "IGNORED" not in clean_screen and "STILL-IGNORED" not in clean_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "edit.txt"
        path.write_bytes(b"abc\r\n")
        code, output, _ = run(
            binary,
            directory,
            [str(path)],
            [(0.25, b"iX\x1b", None), (0.2, b":wq\r", None)],
        )
        assert code == 0, code
        assert path.read_bytes() == b"Xabc\r\n"
        assert_restored(output)

    print("Dust PTY smoke tests passed")


if __name__ == "__main__":
    main()
