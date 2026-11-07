#!/usr/bin/env python3
"""Linux/macOS PTY smoke tests for Dust's real terminal lifecycle and editing."""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import pathlib
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import unicodedata
from collections.abc import Callable

Stage = tuple[float, bytes, tuple[int, int] | None]


def rust_analyzer_is_rustup_proxy() -> bool:
    executable = shutil.which("rust-analyzer")
    if executable is None:
        return False
    path = pathlib.Path(executable)
    try:
        if path.resolve(strict=True).name == "rustup":
            return True
        return path.samefile(path.with_name("rustup"))
    except OSError:
        return False


def rust_analyzer_works() -> bool:
    executable = shutil.which("rust-analyzer")
    if executable is None:
        return False
    try:
        return subprocess.run(
            [executable, "--version"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
        ).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def is_nixos_host() -> bool:
    try:
        values = pathlib.Path("/etc/os-release").read_text(encoding="utf-8").splitlines()
    except OSError:
        return False
    return any(line.removeprefix("ID=").strip('"') == "nixos" for line in values if line.startswith("ID="))


def set_size(descriptor: int, rows: int, columns: int) -> None:
    fcntl.ioctl(descriptor, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def remember_workspace_without_trusting(cwd: str, workspace: pathlib.Path) -> None:
    root = workspace.resolve()
    state = pathlib.Path(cwd) / ".dust-state"
    target = state / "trusted-workspaces.toml"
    encoded_root = json.dumps(str(root))
    existing = target.read_text(encoding="utf-8") if target.exists() else ""
    if encoded_root in existing:
        return
    metadata = root.stat()
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    prefix = existing.rstrip() if existing else "version = 2\nidentities = []"
    target.write_text(
        "\n".join(
            [
                prefix,
                "",
                "[[seen]]",
                f"canonical_path = {encoded_root}",
                f"filesystem_identity = [{metadata.st_dev}, {metadata.st_ino}]",
                "",
            ]
        ),
        encoding="utf-8",
    )


def run(
    binary: pathlib.Path,
    cwd: str,
    arguments: list[str],
    stages: list[Stage],
    *,
    clean: bool = True,
    config_home: pathlib.Path | None = None,
    stage_actions: dict[int, Callable[[], None]] | None = None,
    stage_expectations: dict[int, Callable[[str], bool]] | None = None,
    seen_workspace: bool = True,
    environment: dict[str, str] | None = None,
    coalesce_resize_input: bool = False,
) -> tuple[int, bytes, list[bytes]]:
    if seen_workspace:
        workspace = pathlib.Path(cwd)
        if len(arguments) == 1:
            candidate = pathlib.Path(arguments[0])
            if not candidate.is_absolute():
                candidate = pathlib.Path(cwd) / candidate
            if candidate.is_dir():
                workspace = candidate
        remember_workspace_without_trusting(cwd, workspace)
    pid, descriptor = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.environ["TERM"] = "xterm-256color"
        os.environ["DUST_CONFIG_HOME"] = str(
            config_home or pathlib.Path(cwd) / ".dust-missing-config"
        )
        os.environ["DUST_STATE_HOME"] = str(pathlib.Path(cwd) / ".dust-state")
        # Terminals run a plain sh: a user's bashrc prompt may set the window title,
        # which the docked terminal shows in place of its own.
        os.environ["SHELL"] = "/bin/sh"
        os.environ["PS1"] = "$ "
        if environment:
            os.environ.update(environment)
        command = [str(binary)]
        if clean:
            command.append("--clean")
        os.execv(str(binary), [*command, *arguments])

    set_size(descriptor, 30, 100)
    output = bytearray()
    snapshots: list[bytes] = []
    answered = 0

    def read() -> None:
        # Answer the device attributes query Dust sends at startup, as a terminal without the
        # kitty keyboard protocol does; unanswered, Dust waits two seconds for a reply.
        nonlocal answered
        output.extend(os.read(descriptor, 65536))
        queries = bytes(output).count(b"\x1b[c")
        if queries > answered:
            os.write(descriptor, b"\x1b[?62c" * (queries - answered))
            answered = queries

    # Start input timing after the first complete paint, not after fork: startup
    # and terminal negotiation can outlast a short stage on a shared build host.
    deadline = time.monotonic() + 5
    while b"\x1b[?2026l" not in output and time.monotonic() < deadline:
        readable, _, _ = select.select([descriptor], [], [], 0.02)
        if readable:
            try:
                read()
            except OSError:
                break
    if b"\x1b[?2026l" not in output:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
        os.close(descriptor)
        raise AssertionError("Dust did not paint its first frame within five seconds")

    for stage_index, (delay, data, size) in enumerate(stages):
        deadline = time.time() + delay
        expected = (stage_expectations or {}).get(stage_index)
        last_read = time.time()
        while time.time() < deadline:
            if (
                expected is not None
                and expected(terminal_screen(bytes(output)))
                and time.time() - last_read >= 0.05
            ):
                break
            readable, _, _ = select.select([descriptor], [], [], 0.02)
            if readable:
                try:
                    read()
                    last_read = time.time()
                except OSError:
                    break
        snapshots.append(bytes(output))
        if stage_actions and stage_index in stage_actions:
            stage_actions[stage_index]()
        if coalesce_resize_input and size is not None:
            # Make input and SIGWINCH ready before the event loop can consume
            # either, reproducing focus+resize batches from multiplexers.
            os.kill(pid, signal.SIGSTOP)
            os.waitpid(pid, os.WUNTRACED)
            try:
                if data:
                    os.write(descriptor, data)
                set_size(descriptor, *size)
            finally:
                os.kill(pid, signal.SIGCONT)
        else:
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
                read()
            except OSError:
                pass
        exited, observed_status = os.waitpid(pid, os.WNOHANG)
        if exited:
            status = observed_status
            break
    if status is None:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
        raise AssertionError(
            "Dust did not exit within five seconds; final terminal screen:\n"
            + terminal_screen(bytes(output))
        )
    return os.waitstatus_to_exitcode(status), bytes(output), snapshots


def assert_resize_with_pending_input(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory() as directory:
        target = pathlib.Path(directory) / "resize-race.txt"
        target.write_text("X" * 210 + "\n", encoding="utf-8")
        widths = [180, 100] * 12
        stages: list[Stage] = [(5.0, b"", None)]
        stages.extend((0.5, b"\x1b[O", (30, width)) for width in widths)
        stages.extend([(0.5, b"", None), (0.1, b":q\r", None)])
        code, output, snapshots = run(
            binary, directory, [str(target)], stages, coalesce_resize_input=True,
            stage_expectations={
                0: lambda screen: "X" * 80 in screen and "opening file" not in screen,
            },
        )
        assert code == 0
        for index, width in enumerate(widths):
            # Each resize/focus invalidation emits a complete frame. Parse
            # this stage only, not every preceding full repaint repeatedly.
            delta = snapshots[index + 2][len(snapshots[index + 1]):]
            screen = terminal_screen(delta, width, 30)
            status = screen.splitlines()[29].rstrip()
            assert "NORMAL" in status and status.endswith("1:1"), (index, width, status)
            assert len(status) == width - 1, (index, width, status)
            assert "X" * (width - 10) in screen
        assert_restored(output)


def terminal_projection(
    data: bytes, width: int = 100, height: int = 30
) -> tuple[str, tuple[int, int]]:
    source = data.decode("utf-8", "replace")
    grid = [[" "] * width for _ in range(height)]
    x = 0
    y = 0
    index = 0
    csi = re.compile(r"\x1b\[([?0-9;:]*)([ -/]*)([@-~])")
    while index < len(source):
        if source[index] == "\x1b":
            if index + 1 < len(source) and source[index + 1] == "[":
                match = csi.match(source, index)
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
                    index = match.end()
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


def confirmation_smoke(binary: pathlib.Path) -> None:
    def confirmation_message(screen: str) -> str:
        lines = screen.splitlines()
        title_row = next(row for row in lines if "Delete this path?" in row)
        left = title_row.index("┌")
        right = title_row.rindex("┐")
        wrapped: list[str] = []
        for row in lines[lines.index(title_row) + 1 :]:
            if len(row) <= right or row[left] != "│":
                break
            interior = row[left + 2 : right - 1]
            if "[ Yes ]" in interior or "Resize to review" in interior:
                break
            wrapped.append(interior)
        return "".join(wrapped).rstrip()

    def target_run(choice: bytes) -> tuple[pathlib.Path, bytes, list[bytes]]:
        directory = tempfile.mkdtemp(prefix="dust-confirmation-")
        root = pathlib.Path(directory)
        target = root / "owned  target.txt"
        target.write_text("owned\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.8, b" fe", None),
                (0.8, b"j", None),
                (0.2, b"d", None),
                (0.3, b"", None),
                (0.2, choice, None),
                (0.5, b"\x1b", None),
                (0.2, b"q", None),
            ],
        )
        assert code == 0
        confirmation = terminal_screen(snapshots[3])
        assert "Delete this path?" in confirmation
        assert "[ Yes ]" in confirmation and "[ No ]" in confirmation
        assert confirmation_message(confirmation) == f'Permanently delete "{target}"?'
        assert_restored(output)
        return target, output, snapshots

    no_target, _, no_snapshots = target_run(b"\r")
    assert no_target.is_file(), "default Enter must choose No"
    assert "owned  target.t" in terminal_screen(no_snapshots[5])
    shutil.rmtree(no_target.parent)

    cancelled_target, _, _ = target_run(b"\x1b")
    assert cancelled_target.is_file(), "Escape must cancel without deleting"
    shutil.rmtree(cancelled_target.parent)

    yes_target, _, _ = target_run(b"\x1b[D\r")
    assert not yes_target.exists(), "explicit Yes must delete the original target"
    shutil.rmtree(yes_target.parent)

    with tempfile.TemporaryDirectory(prefix="dust-confirmation-small-") as directory:
        root = pathlib.Path(directory)
        target = root / "small target.txt"
        target.write_text("owned\n", encoding="utf-8")

        def assert_positive_was_refused() -> None:
            assert target.is_file(), "hidden positive action must fail closed"

        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.8, b" fe", None),
                (0.8, b"j", None),
                (0.2, b"", (4, 100)),
                (0.3, b"d", None),
                (0.3, b"\x1b[D\r", None),
                (0.3, b"", (30, 100)),
                (0.3, b"\r", None),
                (0.5, b"\x1b", None),
                (0.2, b"q", None),
            ],
            stage_actions={5: assert_positive_was_refused},
        )
        assert code == 0
        constrained = terminal_screen(snapshots[5], height=4)
        assert "Resize to review" in constrained
        assert "[ Yes ]" not in constrained
        resized = terminal_screen(snapshots[6])
        assert confirmation_message(resized) == f'Permanently delete "{target}"?'
        assert "[ Yes ]" in resized and "[ No ]" in resized
        assert not target.exists(), "Yes must work after the complete target becomes visible"
        assert_restored(output)

    print("Dust owned confirmation PTY tests passed")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, default=pathlib.Path("target/debug/dust"))
    parser.add_argument(
        "--confirmation-only",
        action="store_true",
        help="run only the owned native confirmation PTY qualification",
    )
    arguments = parser.parse_args()
    binary = arguments.binary.resolve()
    if not binary.is_file():
        raise SystemExit(f"Dust binary does not exist: {binary}; run `cargo build`")
    if arguments.confirmation_only:
        confirmation_smoke(binary)
        return

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(1.0, b"\x1b", None), (0.5, b":q\r", None)],
            seen_workspace=False,
        )
        assert code == 0
        first_open = terminal_screen(snapshots[0])
        assert "Do you want to trust this workspace?" in first_open
        assert "LSP servers" in first_open
        assert "[[seen]]" in (
            pathlib.Path(directory) / ".dust-state" / "trusted-workspaces.toml"
        ).read_text(encoding="utf-8")
        assert_restored(output)

        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.8, b":q\r", None)],
            seen_workspace=False,
        )
        assert code == 0
        assert "Do you want to trust this workspace?" not in terminal_screen(snapshots[0])
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [(0.8, b":", None), (0.25, b"q\r", None)],
        )
        assert code == 0, code
        dashboard = terminal_screen(snapshots[0])
        assert pathlib.Path(directory).as_posix() in dashboard
        assert pathlib.Path(directory).name in dashboard
        assert "package activations complete" not in dashboard
        command_screen = terminal_screen(snapshots[1])
        assert "package activations complete" not in command_screen
        assert "Command" in command_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.8, b" fe", None),
                (0.8, b":", None),
                (0.3, b"\x1b", None),
                (0.2, b"\x1b", None),
                (0.2, b"q", None),
            ],
        )
        assert code == 0
        explorer_screen = terminal_screen(snapshots[1])
        assert pathlib.Path(directory).name in explorer_screen
        assert "Workspace files" not in explorer_screen
        command_screen = terminal_screen(snapshots[2])
        assert "Command" in command_screen and "buffer" in command_screen
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        (root / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        (root / "Cargo.toml").write_text(
            '[package]\nname = "menu-probe"\nversion = "0.1.0"\n', encoding="utf-8"
        )
        code, output, snapshots = run(
            binary,
            directory,
            ["main.rs"],
            [
                (5.0, b":lsp\r", None),
                (5.0, b"p", None),
                (5.0, b"", None),
                (0.1, b"\x1b", None),
                (0.1, b"\x1b", None),
                (0.1, b":q\r", None),
            ],
            stage_expectations={
                0: lambda screen: "fn main() {}" in screen and "main.rs" in screen,
                1: lambda screen: " Tools " in screen and "rust-analyzer" in screen,
                2: lambda screen: "Tools · rust-analyzer · choose build" in screen,
            },
        )
        assert code == 0
        tool_screen = terminal_screen(snapshots[1])
        assert " Tools " in tool_screen, repr(tool_screen)
        assert "rust-analyzer" in tool_screen
        assert all(column in tool_screen for column in ("NAME", "VERSION", "SOURCE", "STATE"))
        runtime_screen = terminal_screen(snapshots[2])
        assert "Tools · rust-analyzer · choose build" in runtime_screen
        assert "Enter use this build" in runtime_screen
        assert all(
            column in runtime_screen
            for column in ("VERSION", "RELEASE", "SOURCE", "STATE", "IN USE")
        )
        assert "PATH" in runtime_screen
        if rust_analyzer_is_rustup_proxy():
            assert "rustup bundled" in runtime_screen
            assert "Rust 1." not in runtime_screen
        if is_nixos_host():
            assert "Dust (Nix)" in runtime_screen
            assert not re.search(r"\bDust\s+not installed", runtime_screen)
            assert "0.3.3025" in runtime_screen and "2026-08-24" in runtime_screen
            assert "0.3.3016" in runtime_screen and "2026-08-17.4" in runtime_screen
            assert "0.3.3008" in runtime_screen and "2026-08-10.1" in runtime_screen
            assert "0.3.2660" not in runtime_screen
        else:
            assert re.search(r"\bDust\s+not installed", runtime_screen)
            assert "Dust (Nix)" not in runtime_screen
            assert "0.3.3025" in runtime_screen and "2026-08-24" in runtime_screen
            assert "0.3.3016" in runtime_screen and "2026-08-17.4" in runtime_screen
            assert "0.3.3008" in runtime_screen and "2026-08-10.1" in runtime_screen
            assert "0.3.2660" not in runtime_screen
        assert "i install" in runtime_screen and "u uninstall" in runtime_screen
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
                (0.1, b"\x1b", None),
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
        (root / "alpha.rs").write_text(
            'fn alpha() {\n    println!("picker preview");\n}\n',
            encoding="utf-8",
        )
        (root / "beta.txt").write_text("second file\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.8, b":workspace.file.find\r", None),
                (1.0, b"", None),
                (0.3, b"\x1b", None),
                (0.1, b"\x1b", None),
                (0.2, b":q\r", None),
            ],
        )
        assert code == 0
        file_picker = terminal_screen(snapshots[1])
        assert "Files" in file_picker and "alpha.rs" in file_picker
        assert "picker preview" in file_picker and "2/2" in file_picker
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
            ["alpha.txt"],
            [
                (0.7, b" /", None),
                (0.3, b"needle", None),
                (1.0, b"", None),
                (0.3, b"\x1b", None),
                (0.1, b"\x1b", None),
                (0.2, b" sR", None),
                (0.4, b"", None),
                (0.1, b"\x03", None),
                (0.2, b":workspace.replace needle changed\r", None),
                (0.6, b"\x1b", None),
                (0.2, b":q\r", None),
            ],
        )
        assert code == 0
        direct_screen = terminal_screen(snapshots[1])
        search_screen = terminal_screen(snapshots[2])
        resumed_screen = terminal_screen(snapshots[6])
        preview_screen = terminal_screen(snapshots[9])
        assert "Grep" in resumed_screen and "2/2" in resumed_screen
        result_row = next(row for row in resumed_screen.splitlines() if "needle alpha" in row and "alpha.txt:1:1" in row)
        assert result_row.index("needle alpha") < result_row.index("alpha.txt:1:1")
        assert "Grep" in direct_screen and "Command" not in direct_screen
        assert "Grep" in search_screen and "alpha.txt" in search_screen
        assert "2/2" in search_screen and "needle alpha" in search_screen
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
                (0.8, b":workspace.trust.status\r", None),
                (3.0, b":workspace.trust\r", None),
                (3.0, b"\x1b", None),
                (3.0, b":workspace.trust\r", None),
                (3.0, b"\r", None),
                (3.0, b":q\r", None),
            ],
            stage_expectations={
                1: lambda screen: "untrusted" in screen,
                2: lambda screen: "Do you want to trust this workspace?" in screen,
                3: lambda screen: "not trusted" in screen,
                4: lambda screen: "Do you want to trust this workspace?" in screen,
                5: lambda screen: "workspace trusted" in screen,
            },
        )
        assert code == 0, (code, output[-4000:])
        assert "untrusted" in terminal_screen(snapshots[1]), terminal_screen(snapshots[1])
        trust_prompt = terminal_screen(snapshots[2])
        assert "Do you want to trust this workspace?" in trust_prompt
        assert "LSP servers" in trust_prompt
        assert "not trusted" in terminal_screen(snapshots[3])
        assert "Do you want to trust this workspace?" in terminal_screen(snapshots[4])
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
        root = pathlib.Path(directory)
        source = root / "main.rs"
        source.write_text("fn main() {\n let x = 1;\n}\n", encoding="utf-8")
        linter = root / "lint.sh"
        linter.write_text(
            '#!/bin/sh\nprintf "main.rs:2:1:error:first error\\nmain.rs:2:2:error:second error\\n"\n',
            encoding="utf-8",
        )
        linter.chmod(0o755)
        config_home = root / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            "\n".join(
                [
                    # Whether rust-analyzer is installed must not change the screen under test.
                    "[lsp]",
                    "enabled = false",
                    "[tasks.check]",
                    'kind = "linter"',
                    f"executable = {json.dumps(str(linter))}",
                    "arguments = []",
                    f"cwd = {json.dumps(directory)}",
                    "[tasks.check.matcher]",
                    "pattern = '(?m)^(?P<path>[^:]+):(?P<line>\\d+):(?P<column>\\d+):(?P<severity>[^:]+):(?P<message>.+)$'",
                    'path_group = "path"',
                    'line_group = "line"',
                    'column_group = "column"',
                    'severity_group = "severity"',
                    'message_group = "message"',
                    "",
                ]
            ),
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            ["main.rs"],
            [
                (0.7, b":workspace.trust\r", None),
                (0.2, b"\r", None),
                (0.6, b":lint.run name=check\r", None),
                (1.0, b"", None),
                (0.2, b"]e", None),
                (0.7, b"", None),
                (0.2, b":q!\r", None),
            ],
            clean=False,
            config_home=config_home,
        )
        assert code == 0
        inline = terminal_screen(snapshots[3])
        assert "●● first error [check]" in inline, inline
        status = inline.splitlines()[29]
        assert "\uf057 2" in status, status
        assert "\uf071" not in status, status  # zero warnings stay hidden
        expanded = terminal_screen(snapshots[5])
        assert "Diagnostics:" in expanded
        assert "1. first error [check]" in expanded
        assert "2. second error [check]" in expanded
        assert_restored(output)

    # Scrollable hover documentation owns real keys instead of trapping an
    # oversized popup over the editor.
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        (root / "Cargo.toml").write_text(
            '[package]\nname = "hover-scroll"\nversion = "0.1.0"\nedition = "2024"\n',
            encoding="utf-8",
        )
        (root / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        bin_dir = root / "bin"
        bin_dir.mkdir()
        server = bin_dir / "rust-analyzer"
        server.write_text(
            r'''#!/usr/bin/env python3
import json, sys
if "--version" in sys.argv:
    print("rust-analyzer 1.99.0 (test 2026-08-29)")
    raise SystemExit(0)
def read_message():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1])
    return json.loads(sys.stdin.buffer.read(length))
def send(value):
    data = json.dumps(value, separators=(",", ":")).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
    sys.stdout.buffer.flush()
while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{"hoverProvider":True,"textDocumentSync":1}}})
    elif method == "textDocument/hover":
        lines = "\n".join(f"hover line {line}" for line in range(40))
        send({"jsonrpc":"2.0","id":message["id"],"result":{"contents":{"kind":"markdown","value":lines}}})
    elif method == "shutdown":
        send({"jsonrpc":"2.0","id":message["id"],"result":None})
    elif method == "exit":
        break
''',
            encoding="utf-8",
        )
        server.chmod(0o755)
        code, output, snapshots = run(
            binary,
            directory,
            ["main.rs"],
            [
                (0.7, b":workspace.trust\r", None),
                (0.2, b"\r", None),
                (1.8, b"K", None),
                (0.8, b"", None),
                (0.1, b"j", None),
                (0.3, b"", None),
                (0.1, b"\x1b", None),
                (0.1, b":q!\r", None),
            ],
            environment={"PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"},
        )
        assert code == 0
        popup = terminal_screen(snapshots[3])
        scrolled = terminal_screen(snapshots[5])
        assert "j/k or wheel" in popup
        first_range = re.search(r"j/k or wheel\s+·\s+(\d+)-", popup)
        second_range = re.search(r"j/k or wheel\s+·\s+(\d+)-", scrolled)
        assert first_range and second_range
        assert int(second_range.group(1)) > int(first_range.group(1))
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        (root / "seed.txt").write_text("seed", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [],
            [
                (0.8, b" fe", None),
                (0.8, b"a", None),
                (0.2, b"new.txt", None),
                (0.2, b"\r", None),
                (0.6, b"r", None),
                (0.2, b"\x7f\x7f\x7f\x7f\x7f\x7f\x7frenamed.txt", None),
                (0.2, b"\r", None),
                (0.6, b"a", None),
                (0.2, b"dest/", None),
                (0.2, b"\r", None),
                (0.6, b"j", None),
                (0.2, b"c", None),
                (0.2, b"k", None),
                (0.2, b"p", None),
                (0.7, b"j", None),
                (0.2, b"d", None),
                (0.3, b"", None),
                (0.2, b"\r", None),  # safe default No
                (0.4, b"d", None),
                (0.2, b"\x1b", None),  # distinct cancellation
                (0.4, b"d", None),
                (0.2, b"\x1b[D", None),  # select Yes
                (0.2, b"\r", None),
                (0.8, b"\x1b", None),
                (0.2, b"q", None),
            ],
        )
        assert code == 0
        confirmation = terminal_screen(snapshots[16])
        assert "Delete this path?" in confirmation
        assert "[ Yes ]" in confirmation and "[ No ]" in confirmation
        assert "renamed.txt" in confirmation
        assert "renamed.txt" in terminal_screen(snapshots[17])  # default Enter kept it
        assert (root / "dest" / "renamed.txt").read_text(encoding="utf-8") == ""
        assert not (root / "renamed.txt").exists()
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
                (1.5, f":e {target}\r".encode(), None),
                (5.0, b":q\r", None),
            ],
            stage_expectations={
                6: lambda screen: "second root" in screen and "from-b.txt" in screen,
            },
        )
        assert code == 0
        assert "added workspace root" in terminal_screen(snapshots[1])
        assert "Projects" in terminal_screen(snapshots[2])
        assert "root-b" in terminal_screen(snapshots[3])
        assert "active root" in terminal_screen(snapshots[5])
        opened = terminal_screen(snapshots[6])
        assert "second root" in opened and "from-b.txt" in opened, opened
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
        assert pathlib.Path(directory).name in terminal_screen(snapshots[1])
        assert "nested" in terminal_screen(snapshots[1])
        assert "inside.txt" in terminal_screen(snapshots[3])
        opened_with_dock = terminal_screen(snapshots[4])
        assert pathlib.Path(directory).name in opened_with_dock
        assert "inside.txt" in opened_with_dock
        assert "explorer pty content" in opened_with_dock
        assert opened_with_dock.splitlines()[0][40] == "│"
        assert_restored(output)

        code, output, snapshots = run(
            binary, directory, [],
            [(0.7,b":explore\r",None),(0.2,b"l",None),(0.7,b"j",None),(0.2,b"s",None),(0.8,b":q!\r",None)],
        )
        assert code == 0
        split_screen = terminal_screen(snapshots[4])
        assert pathlib.Path(directory).name in split_screen and "inside.txt" in split_screen
        assert "explorer pty content" in split_screen
        assert split_screen.splitlines()[0][40] == "│"
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
        # The search box keeps its prompt when unfocused; the real terminal
        # cursor moves from the explorer's left margin into the editor.
        focused_explorer = terminal_cursor(snapshots[1])
        focused_editor = terminal_cursor(snapshots[3])
        refocused_explorer = terminal_cursor(snapshots[5])
        assert focused_explorer[0] == 0, focused_explorer
        assert 41 <= focused_editor[0] < 100, focused_editor
        assert refocused_explorer == focused_explorer, refocused_explorer
        assert_restored(output)

    # Screen-painting movements must never be exposed as visible cursor
    # movement (notably with Kitty cursor trails). Scroll through real frames.
    with tempfile.TemporaryDirectory() as directory:
        target = pathlib.Path(directory) / "cursor-trails.txt"
        target.write_text("\n".join(f"line {line}" for line in range(200)) + "\n")
        code, output, snapshots = run(
            binary,
            directory,
            [str(target)],
            [
                (0.7, b"\x04", None),  # Ctrl-d
                (0.3, b"\x04", None),
                (0.3, b"\x15", None),  # Ctrl-u
                (0.3, b":q\r", None),
            ],
        )
        assert code == 0
        frames = re.findall(rb"\x1b\[\?2026h(.*?)\x1b\[\?2026l", output, re.S)
        assert len(frames) >= 4
        for frame in frames:
            assert frame.startswith(b"\x1b[?25l")
            assert frame.count(b"\x1b[?25h") <= 1
            if b"\x1b[?25h" in frame:
                # Cursor shown only after its final position and shape.
                assert re.search(rb"\x1b\[\d+;\d+H\x1b\[\d+ q\x1b\[\?25h$", frame)
        assert_restored(output)

    # SIGWINCH-driven resizing must repaint without accompanying key input,
    # including newly exposed cells after shrinking and growing again.
    with tempfile.TemporaryDirectory() as directory:
        target = pathlib.Path(directory) / "grow.txt"
        target.write_text("X" * 170 + "\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(target)],
            [
                (5.0, b"", (30, 180)),
                (0.7, b"", (18, 60)),
                (0.7, b"", (30, 180)),
                (0.7, b"", None),
                (0.1, b":q\r", None),
            ],
            # The opening notice floats over the first line; let it fade before
            # checking how much file content each resized viewport exposes.
            stage_expectations={
                0: lambda screen: "X" * 80 in screen and "opening file" not in screen,
            },
        )
        assert code == 0
        for snapshot in (snapshots[1], snapshots[3]):
            grown = terminal_screen(snapshot, 180, 30)
            assert "X" * 120 in grown, repr(grown)
            assert "NORMAL" in grown.splitlines()[29], repr(grown)
        shrunk = terminal_screen(snapshots[2], 60, 18)
        assert "X" * 40 in shrunk, repr(shrunk)
        assert "NORMAL" in shrunk.splitlines()[17], repr(shrunk)
        assert_restored(output)

    assert_resize_with_pending_input(binary)

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
                (5.0, f":edit {third}\r".encode(), None),
                (5.0, b":explore\r", None),
                (5.0, b"\x0c", None),  # explorer -> editor
                (0.3, b"H", None),
                (0.3, b"L", None),
                (0.3, b":q\r", None),
            ],
            stage_expectations={
                1: lambda screen: "BETA-CONTENT" in screen,
                2: lambda screen: "GAMMA-CONTENT" in screen,
                3: lambda screen: "Explorer" in screen.splitlines()[0][:41]
                and pathlib.Path(directory).name in "\n".join(
                    row[:40] for row in screen.splitlines()
                )
                and all(
                    name in screen.splitlines()[0][41:]
                    for name in ("alpha.rs", "beta.rs", "gamma.rs")
                ),
            },
        )
        assert code == 0
        opened = terminal_screen(snapshots[3])
        top = opened.splitlines()[0]
        assert "Explorer" in top[:41], top
        assert pathlib.Path(directory).name in "\n".join(
            row[:40] for row in opened.splitlines()
        ), opened
        assert all(name in top[41:] for name in ("alpha.rs", "beta.rs", "gamma.rs")), top
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
                (0.2, b"\r", None),
                (0.5, b":explore\r", None),
                (0.7, b"\x0c", None),  # explorer -> editor
                (0.7, b"\x1f", None),  # legacy Ctrl+/ opens bundled shell
                (0.3, b"stty -echo\r", None),
                (
                    1.0,
                    b"printf '%b' '\\033\\133?2004h\\033\\1331;32mDOCKED-TERMINAL\\033\\1330m\\033\\1350;title\\a\\n'\r",
                    None,
                ),
                (1.5, b"\x1f", None),  # allow bounded PTY output, then hide the focused terminal
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
        initial_terminal = terminal_screen(snapshots[6], 100, 30)
        assert "sh ·" in initial_terminal.splitlines()[19], initial_terminal
        assert pathlib.Path(directory).name in initial_terminal.splitlines()[19], initial_terminal
        assert "Normal mode" in initial_terminal.splitlines()[19], initial_terminal
        focused = terminal_screen(snapshots[7], 100, 30)
        focused_rows = focused.splitlines()
        assert pathlib.Path(directory).name in focused and "EDITOR-TOP" in focused
        # The ten-row dock ends above the screen's persistent status line.
        assert "title" in focused_rows[19]
        assert any("DOCKED-TERMINAL" in row for row in focused_rows[20:29])
        assert "[?2004h" not in focused and "[1;32m" not in focused
        assert "]0;title" not in focused
        assert "TERMINAL" in focused_rows[29]
        assert "TERMINAL" not in focused_rows[19]
        assert "unsupported" not in focused_rows[29]
        assert focused_rows[20][40] != "│"  # terminal spans beneath explorer

        hidden = terminal_screen(snapshots[8], 100, 30)
        assert "title" not in hidden
        assert "unsupported" not in hidden
        assert "EDITOR-TOP" in hidden
        assert terminal_cursor(snapshots[8], 100, 30)[1] < 30

        resized = terminal_screen(snapshots[9], 60, 20)
        assert "title" not in resized
        assert "EDITOR-TOP" in resized
        refocused = terminal_screen(snapshots[10], 60, 20)
        assert "title" in refocused
        assert 10 <= terminal_cursor(snapshots[10], 60, 20)[1] < 19
        exited = terminal_screen(snapshots[11], 60, 20)
        assert "title" not in exited
        assert "EXIT 0" not in exited
        assert "EDITOR-TOP" in exited
        assert_restored(output)

    # A rustup proxy without the rust-analyzer component must fail closed
    # instead of entering a permanent STARTING/restart loop.
    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        (root / "Cargo.toml").write_text(
            "[package]\nname = \"broken-rustup-proxy\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            encoding="utf-8",
        )
        (root / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
        bin_dir = root / "bin"
        bin_dir.mkdir()
        rustup = bin_dir / "rustup"
        rustup.write_text(
            """#!/bin/sh
if [ "$1" = "component" ]; then
  touch "$PWD/.component-installed"
  exit 0
fi
if [ "$1" = "--version" ] && [ -f "$PWD/.component-installed" ]; then
  echo 'rust-analyzer 1.99.0 (test 2026-08-29)'
  exit 0
fi
echo 'rust-analyzer is unavailable for the active toolchain' >&2
exit 1
""",
            encoding="utf-8",
        )
        rustup.chmod(0o755)
        os.link(rustup, bin_dir / "rust-analyzer")
        config_home = root / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            "[lsp]\nauto_start = false\nauto_install = false\n",
            encoding="utf-8",
        )
        code, output, snapshots = run(
            binary,
            directory,
            ["main.rs"],
            [
                (0.5, b":workspace.trust\r", None),
                (0.2, b"\r", None),
                (1.2, b":lsp\r", None),
                (0.8, b"i", None),
                (1.5, b"", None),
                (0.1, b"\x1b", None),
                (0.1, b":q\r", None),
            ],
            clean=False,
            config_home=config_home,
            environment={"PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}"},
        )
        assert code == 0
        manager = terminal_screen(snapshots[3])
        assert re.search(r"rust-analyzer[^\n]*PATH\s+failed", manager), manager
        assert "starting" not in manager
        installed = terminal_screen(snapshots[4])
        assert re.search(r"rust-analyzer[^\n]*PATH\s+installed", installed), installed
        assert "installed rust-analyzer in the active rustup toolchain" in installed
        assert (root / ".component-installed").is_file()
        assert_restored(output)

    if rust_analyzer_works():
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "Cargo.toml").write_text(
                "[package]\nname = \"dust-pty-lsp\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
                encoding="utf-8",
            )
            path = root / "main.rs"
            path.write_text("fn main() {}\n", encoding="utf-8")
            code, output, snapshots = run(
                binary,
                directory,
                [str(path)],
                [(0.4,b":workspace.trust\r",None),(0.2,b"\r",None),(1.7,b"",None),(0.1,b":lsp.server.status\r",None),(0.2,b":q\r",None)],
            )
            assert code == 0
            lsp_status = terminal_screen(snapshots[4])
            assert "rust-analyzer" in lsp_status
            assert "root:" not in lsp_status
            assert_restored(output)

    for executable, suffix, source, server, marker, marker_source in [
        (
            "typescript-language-server",
            ".ts",
            "const value: number = 1;\n",
            "typescript-language-server",
            "package.json",
            "{\"name\": \"dust-pty-lsp\", \"private\": true}\n",
        ),
        (
            "gopls",
            ".go",
            "package main\nfunc main() {}\n",
            "gopls",
            "go.mod",
            "module example.com/dust-pty-lsp\n\ngo 1.20\n",
        ),
    ]:
        if not shutil.which(executable):
            continue
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / marker).write_text(marker_source, encoding="utf-8")
            path = root / f"main{suffix}"
            path.write_text(source, encoding="utf-8")
            code, output, snapshots = run(
                binary,
                directory,
                [str(path)],
                [(0.4,b":workspace.trust\r",None),(0.2,b"\r",None),(1.7,b"",None),(0.1,b":lsp.server.status\r",None),(0.2,b":q\r",None)],
            )
            assert code == 0
            lsp_status = terminal_screen(snapshots[4])
            assert server in lsp_status
            assert "root:" not in lsp_status
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
            [(0.5, b":q\r", None)],
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
            "[ui]\nmouse=false\n",
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
        # Alternate screen, bracketed paste and focus events are always enabled;
        # mouse capture remains configurable.
        assert_restored(output)
        assert b"\x1b[?1000h" not in output
        assert b"\x1b[?1004h" in output and b"\x1b[?1004l" in output

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
                (0.2, b"\r", None),
                (0.2, b":q!\r", None),
            ],
        )
        assert code == 0
        recovery_picker = terminal_screen(snapshots[1])
        assert "Recover Unsaved Work" in recovery_picker, recovery_picker
        assert "1/1" in recovery_picker and "recover-me.txt" in recovery_picker
        assert "unsaved-base" in recovery_picker
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
        assert re.search(r"\bQuit +z *$", remapped, re.M), remapped
        assert not re.search(r"\bQuit +q *$", remapped, re.M)
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
        assert re.search(r"\bQuit +q *$", clean_screen, re.M), clean_screen
        assert not re.search(r"\bQuit +z *$", clean_screen, re.M)
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        config_home = pathlib.Path(directory) / "config"
        config_home.mkdir()
        (config_home / "config.toml").write_text(
            """
[input.keymaps]
leader = ","
local_leader = ";"
""".strip(),
            encoding="utf-8",
        )
        (config_home / "keymaps.toml").write_text(
            """
[[keymap]]
id = "builtin.dashboard.surface.editor-quit.q"
disable = true

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
        assert re.search(r"\bQuit +, q *$", leader_screen, re.M), leader_screen
        assert not re.search(r"\bQuit +q *$", leader_screen, re.M)
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
        assert re.search(r"\bQuit +q *$", clean_leader_screen, re.M), clean_leader_screen
        assert not re.search(r"\bQuit +, q *$", clean_leader_screen, re.M)
        assert_restored(output)

    with tempfile.TemporaryDirectory() as directory:
        root = pathlib.Path(directory)
        path = root / "hints.txt"
        path.write_text("key hint smoke\n", encoding="utf-8")
        code, output, snapshots = run(
            binary,
            directory,
            [str(path)],
            [
                (0.5, b" ", None),
                (0.9, b"\x1b", None),
                (0.3, b"g", None),
                (0.9, b"\x1b", None),
                (0.1, b":q\r", None),
            ],
            clean=True,
        )
        assert code == 0
        hint_screen = terminal_screen(snapshots[1])
        assert "file/find" in hint_screen
        assert "search" in hint_screen
        assert "diagnostics/" in hint_screen
        assert "code" in hint_screen
        assert "Keys: space" in hint_screen
        goto_screen = terminal_screen(snapshots[3])
        assert "Go to implementation" in goto_screen
        assert "Go to type definition" in goto_screen
        assert "Show signature help" in goto_screen
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
id = "builtin.global.group.files.editor-normal"
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
        assert "file/find" not in custom_hint_screen
        assert "Keys: ," in custom_hint_screen
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
        assert "file/find" not in disabled_hint_screen
        assert "diagnostics/" not in disabled_hint_screen
        assert "Keys: space" not in disabled_hint_screen
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
content_width = 66
minimum_width = 20
minimum_height = 7

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
                (5.0, b":config reload\r", None),
                (5.0, b"p", None),
                (0.2, b"\x1b", None),
                (0.2, b":bye\r", None),
            ],
            clean=False,
            config_home=config_home,
            stage_actions={3: lambda: config_path.write_text("[ui\n", encoding="utf-8")},
            stage_expectations={
                3: lambda screen: "configuration reloaded" in screen,
                4: lambda screen: "reload rejected" in screen,
            },
        )
        assert code == 0
        assert "CUSTOM DASH" in terminal_screen(snapshots[0])
        assert "OPS CONSOLE" in terminal_screen(snapshots[1])
        reloaded = terminal_screen(snapshots[3])
        rejected = terminal_screen(snapshots[4])
        assert "configuration reloaded" in reloaded, repr(reloaded)
        assert "reload rejected" in rejected, repr(rejected)
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
            [(0.8, b"z", None)],
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
                "[ui]\n"
                f"theme='{theme}'\n"
                "[ui.status_line.mode_labels]\n"
                f"normal='{label}'\n"
            )

        config_path.write_text(watched_config("WATCH-OLD"), encoding="utf-8")

        watched_dashboard = (
            repository / "assets/defaults/dashboard.toml"
        ).read_text(encoding="utf-8").replace("D U S T", "WATCHED DASHBOARD")
        # Keep the watched identity visible instead of the bundled large logo.
        watched_dashboard = re.sub(
            r'logo = """.*?"""', 'logo = ""', watched_dashboard, count=1, flags=re.S
        )

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
                (0.8, b"", None),
                (5.0, b"", None),
                (5.0, b"", None),
                (4.0, b"", None),
                (5.0, b":bd\r", None),
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
            stage_expectations={
                1: lambda screen: "WATCH-NEW" in screen,
                2: lambda screen: "WATCH-THEME2" in screen,
                4: lambda screen: "WATCH-THEME2" in screen
                and "automatic configuration reload rejected" in screen,
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
