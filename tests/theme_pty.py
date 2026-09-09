#!/usr/bin/env python3
"""Owned-PTY theme preview/cancel/session-apply probe; no installed tools needed."""
import argparse
import pathlib
import tempfile

from pty_smoke import run, terminal_screen


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    binary = parser.parse_args().binary.resolve()
    theme = (pathlib.Path(__file__).resolve().parents[1] / "assets/themes/dust.toml").read_text()
    with tempfile.TemporaryDirectory(prefix="dust-theme-pty-") as directory:
        root = pathlib.Path(directory)
        config = root / "config"
        (config / "themes").mkdir(parents=True)
        settings = '[ui]\ncolor_mode = "true_color"\n[config_watch]\nenabled = false\n'
        (config / "config.toml").write_text(settings)
        (config / "themes/theme-probe.toml").write_text(
            theme.replace('name = "dust"', 'name = "theme-probe"').replace('"#c9d1d9"', '"#010203"')
        )
        code, output, snapshots = run(
            binary, directory, [],
            [(1.0, b":theme.select\r", None),
             (0.7, b"theme-probe", None),
             (0.7, b"\x1b", None),
             (0.7, b":theme.select\r", None),
             (0.7, b"theme-probe", None),
             (0.7, b"\r", None),
             (0.7, b":q\r", None)],
            clean=False, config_home=config,
        )
        assert code == 0, output[-2000:]
        preview = b"\x1b[38;2;1;2;3m"
        original = b"\x1b[38;2;201;209;217m"
        assert "Select theme" in terminal_screen(snapshots[2])
        assert "theme-probe" in terminal_screen(snapshots[2])
        assert preview in snapshots[2][len(snapshots[1]):], "preview did not reach the terminal painter"
        cancelled = snapshots[3][len(snapshots[2]):]
        assert original in cancelled and preview not in cancelled, "Escape did not restore accepted colors"
        assert "Select theme" not in terminal_screen(snapshots[3])
        applied = snapshots[6][len(snapshots[5]):]
        assert preview in applied, "Enter did not preserve the session theme"
        assert "default not saved" in terminal_screen(snapshots[6])
        assert (config / "config.toml").read_text() == settings
        assert b"\x1b[?1049l" in output, "alternate screen not restored"
    print("theme PTY: preview, Escape rollback, session apply, unchanged configuration, clean exit passed")


if __name__ == "__main__":
    main()
