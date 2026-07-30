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


def terminal_screen(data: bytes, width: int = 100, height: int = 30) -> str:
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
    return "\n".join("".join(row) for row in grid)


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
        assert "explorer pty content" in terminal_screen(snapshots[4])
        assert_restored(output)

        code, output, snapshots = run(
            binary, directory, [],
            [(0.7,b":explore\r",None),(0.2,b"l",None),(0.7,b"j",None),(0.2,b"s",None),(0.8,b":q!\r",None)],
        )
        assert code == 0
        split_screen = terminal_screen(snapshots[4])
        assert "explorer pty content" in split_screen
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
context = "dashboard"
keys = "q"
unmap = true

[[keymap]]
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
context = "dashboard"
keys = "p"
command = "command_line.open"
arguments = { kind = "command" }

[[keymap]]
context = "dashboard"
keys = "z"
command = "editor.quit"
arguments = { force = false }

[[keymap]]
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
            '[styles.accent]\nforeground = "cyan"',
            '[styles.accent]\nforeground = "indexed:201"\nforeground_16 = "cyan"',
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
                (0.5, b"p", None),
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
            '[styles.accent]\nforeground = "cyan"',
            '[styles.accent]\nforeground = "indexed:202"\nforeground_16 = "cyan"',
        )
        (themes / "watched.toml").write_text(watched_theme, encoding="utf-8")
        (config_home / "keymaps.toml").write_text(
            "[[keymap]]\ncontext='editor'\nmode='normal'\nkeys='z'\ncommand='editor.quit'\narguments={force=false}\n"
            "[[keymap]]\ncontext='dashboard'\nkeys='z'\ncommand='editor.quit'\narguments={force=false}\n",
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
                "[[keymap]]\ncontext='editor'\nmode='normal'\nkeys='y'\ncommand='editor.quit'\narguments={force=false}\n"
                "[[keymap]]\ncontext='dashboard'\nkeys='y'\ncommand='editor.quit'\narguments={force=false}\n",
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
                (0.4, b"", None),
                (0.4, b":bd\r", None),
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
