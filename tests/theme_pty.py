#!/usr/bin/env python3
"""Owned-PTY compact theme chooser, arrow preview, cancel and session-apply probe."""
import argparse
import json
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
        source = root / "preview.txt"
        source.write_text("\n".join(f"THEME_PREVIEW_SOURCE_{line:02}" for line in range(80)) + "\n")
        code, output, snapshots = run(
            binary, directory, [str(source)],
            [(1.0, b":theme.select\r", (70, 220)),
             (0.7, b"\x1b[B", None),
             (0.7, b"\x1b[A", None),
             (0.7, b"theme-probe", None),
             (0.7, b"\x1b", None),
             (0.7, b":theme.select\r", None),
             (0.7, b"theme-probe", None),
             (0.7, b"\r", None),
             (0.7, b":q\r", None)],
            clean=False, config_home=config,
        )
        assert code == 0, output[-2000:]
        screen = lambda index: terminal_screen(snapshots[index], 220, 70)
        delta = lambda index: snapshots[index][len(snapshots[index - 1]):]
        choices = screen(1)
        for label in ["Dust", "Catppuccin Latte", "Catppuccin Frappé", "Catppuccin Macchiato", "Catppuccin Mocha", "Gruvbox Dark", "Nord"]:
            assert label in choices, (label, choices)
        assert sum("Dust" in line and "Current" in line for line in choices.splitlines()) == 1
        assert "<bundled:" not in choices
        assert "THEME_PREVIEW_SOURCE_00" in choices and "THEME_PREVIEW_SOURCE_20" in choices
        # Even in a 220x70 terminal the dialog is only 64x12 cells, near the top.
        assert choices.splitlines()[4].index("Dust themes") == 81, choices
        assert "Live preview" in choices.splitlines()[15], choices
        assert "escape cancel" in choices.splitlines()[15], choices
        preview = b"\x1b[38;2;1;2;3m"
        original = b"\x1b[38;2;201;209;217m"
        assert b"\x1b[38;2;76;79;105m" in delta(2), "Down did not preview Catppuccin Latte"
        assert original in delta(3), "Up did not preview Dust"
        assert "Dust themes" in screen(4) and "theme-probe" in screen(4)
        assert preview in delta(4), "installed-theme preview did not reach the painter"
        assert original in delta(5) and preview not in delta(5), "Escape did not restore accepted colors"
        assert "Dust themes" not in screen(5)
        assert preview in delta(8), "Enter did not preserve the session theme"
        assert "editor-state save requested" in screen(8)
        assert (config / "config.toml").read_text() == settings
        assert b"\x1b[?1049l" in output, "alternate screen not restored"
        state_path = root / ".dust-state/session.json"
        saved = json.loads(json.loads(state_path.read_text())["metadata"]["ui.theme.snapshot"])
        assert saved["name"] == "theme-probe"
        (config / "themes/theme-probe.toml").unlink()  # Saved colors do not require re-reading an installed file.
        code, output, restored = run(binary, directory, [str(source)], [
            (1.0, b":theme.select\r", None),
            (0.7, b"\x1b[B", None),
            (0.7, b"\x1b", None),
            (0.7, b":q\r", None),
        ], clean=False, config_home=config)
        assert code == 0
        assert preview in restored[0], "saved theme was not restored at launch"
        assert "theme-probe" in terminal_screen(restored[1])
        assert original in restored[2][len(restored[1]):], "Down did not preview Dust after restart"
        assert preview in restored[3][len(restored[2]):], "Escape did not restore the remembered theme"
        assert json.loads(json.loads(state_path.read_text())["metadata"]["ui.theme.snapshot"]) == saved
        assert (config / "config.toml").read_text() == settings
    print("theme PTY: compact 64x12 chooser, seven bundled themes, Up/Down live preview, Escape rollback, state save/restart, cancelled preview not persisted and clean exit passed")


if __name__ == "__main__":
    main()
