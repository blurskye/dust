#!/usr/bin/env python3
"""Isolated owner-boundary regressions using Dust's real terminal adapter."""
import argparse
import pathlib
import tempfile

from pty_smoke import assert_restored, run, terminal_screen


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, default=pathlib.Path("target/debug/dust"))
    binary = parser.parse_args().binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dust-owner-notice-") as directory:
        code, output, snapshots = run(
            binary, directory, ["--safe-mode"],
            [
                (1.5, b":application.safe_mode.status\r", None),
                (0.6, b":application.notifications.open\r", None),
                (0.6, b"\x1b", None),
                (0.3, b"q", None),
            ],
        )
        assert code == 0, code
        history = terminal_screen(snapshots[2])
        assert "Notifications" in history, history
        assert "safe mode" in history.lower(), history
        assert_restored(output)

    with tempfile.TemporaryDirectory(prefix="dust-owner-syntax-") as directory:
        source = pathlib.Path(directory) / "sample.rs"
        source.write_text("fn main() {\n    let value = 42;\n}\n", encoding="utf-8")
        code, output, snapshots = run(
            binary, directory, ["--safe-mode", str(source)],
            [
                (1.5, b":syntax.status\r", None),
                (0.6, b"\x1b", None),
                (0.3, b":q\r", None),
            ],
        )
        assert code == 0, code
        status = terminal_screen(snapshots[1])
        assert "Rust" in status and "active" in status, status
        assert_restored(output)
    with tempfile.TemporaryDirectory(prefix="dust-owner-input-") as directory:
        root = pathlib.Path(directory)
        source = root / "input.txt"
        source.write_text("input owner\n", encoding="utf-8")
        config = root / "config"
        config.mkdir()

        def configure(marker: str) -> None:
            (config / "config.toml").write_text(
                '[config_watch]\nenabled = false\n'
                '[ui.key_hints]\ndelay_ms = 0\n'
                f'[messages]\nreloaded = "{marker}"\n',
                encoding="utf-8",
            )

        configure("OLD_CONFIG_MARKER")
        code, output, snapshots = run(
            binary, directory, ["--safe-mode", str(source)],
            [
                (0.8, b" ", None),
                (0.8, b"f", None),
                (0.25, b"b", None),
                (0.3, b"\x1b", None),
                (0.2, b":config.reload\r", None),
                (0.8, b":q\r", None),
            ],
            clean=False, config_home=config,
            stage_actions={4: lambda: configure("OWNER_CONFIG_ACCEPTED")},
        )
        assert code == 0, code
        assert "space" in terminal_screen(snapshots[1])
        assert "space f" in terminal_screen(snapshots[2])
        assert "Buffers" in terminal_screen(snapshots[3])
        assert "OWNER_CONFIG_ACCEPTED" in terminal_screen(snapshots[5])
        assert_restored(output)
    with tempfile.TemporaryDirectory(prefix="dust-owner-replay-") as directory:
        source = pathlib.Path(directory) / "replay.txt"
        source.write_text("abcdef\n", encoding="utf-8")
        code, output, _ = run(
            binary, directory, ["--safe-mode", str(source)],
            [
                (1.5, b"qaxq", None),
                (0.3, b"@a", None),
                (0.3, b"@@", None),
                (0.3, b".", None),
                (0.3, b":w\r", None),
                (0.4, b":q\r", None),
            ],
        )
        assert code == 0, code
        assert source.read_text(encoding="utf-8") == "ef\n"
        assert_restored(output)
    print("Dust ownership PTY regressions passed")


if __name__ == "__main__":
    main()
