use std::collections::{BTreeMap, BTreeSet};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::Config;
use super::settings::{AmbiguousWidth, TaskConfig, TaskKind, WindowPlacementConfig};

fn task_graph_has_cycle(tasks: &BTreeMap<String, TaskConfig>) -> bool {
    fn visit(
        name: &str,
        tasks: &BTreeMap<String, TaskConfig>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> bool {
        if visited.contains(name) {
            return false;
        }
        if !visiting.insert(name.to_owned()) {
            return true;
        }
        if tasks.get(name).is_some_and(|task| {
            task.dependencies
                .iter()
                .any(|dependency| visit(dependency, tasks, visiting, visited))
        }) {
            return true;
        }
        visiting.remove(name);
        visited.insert(name.to_owned());
        false
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    tasks
        .keys()
        .any(|name| visit(name, tasks, &mut visiting, &mut visited))
}

impl Config {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        const SUPPORTED_WINDOW_CONTENT: &[&str] = &[
            "builtin.explorer",
            "builtin.terminal",
            "builtin.dashboard",
            "builtin.picker",
            "builtin.problems",
            "builtin.output",
            "builtin.logs",
            "builtin.debug",
            "builtin.theme",
        ];
        if self.windows.rules.len() > 32 {
            return Err("windows.rule exceeds the 32-rule ceiling");
        }
        let mut window_targets = BTreeSet::new();
        let mut fullscreen_rules = 0_usize;
        for rule in &self.windows.rules {
            let plugin_content = rule.content_type.starts_with("plugin.")
                && rule.content_type.len() <= 128
                && rule.content_type.split('.').count() >= 3
                && rule.content_type.split('.').all(|segment| {
                    segment
                        .bytes()
                        .next()
                        .is_some_and(|byte| byte.is_ascii_lowercase())
                        && segment.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        })
                });
            if !SUPPORTED_WINDOW_CONTENT.contains(&rule.content_type.as_str()) && !plugin_content {
                return Err("windows.rule contains an unsupported content_type");
            }
            if !window_targets.insert(rule.content_type.as_str()) {
                return Err("windows.rule contains a duplicate content_type");
            }
            match &rule.placement {
                WindowPlacementConfig::Automatic | WindowPlacementConfig::Hidden => {}
                WindowPlacementConfig::Docked { edge, size } => {
                    if !matches!(edge.as_str(), "left" | "right" | "top" | "bottom")
                        || !(1..=240).contains(size)
                    {
                        return Err("windows.rule contains an invalid dock placement");
                    }
                }
                WindowPlacementConfig::Floating { width, height, .. } => {
                    if *width == 0 || *height == 0 {
                        return Err("windows.rule contains an invalid floating placement");
                    }
                }
                WindowPlacementConfig::Fullscreen => fullscreen_rules += 1,
            }
        }
        if fullscreen_rules > 1 {
            return Err("windows.rule contains conflicting fullscreen defaults");
        }
        if !(1..=32).contains(&self.editor.tab_width) {
            return Err("editor.tab_width must be between 1 and 32");
        }
        if !(1..=32).contains(&self.editor.indent_width) {
            return Err("editor.indent_width must be between 1 and 32");
        }
        for (name, sequence) in [
            ("input.keymaps.leader", &self.input.keymaps.leader),
            (
                "input.keymaps.local_leader",
                &self.input.keymaps.local_leader,
            ),
        ] {
            let tokens = sequence.split_whitespace().collect::<Vec<_>>();
            if tokens.is_empty() {
                return Err(match name {
                    "input.keymaps.leader" => "input.keymaps.leader must not be empty",
                    _ => "input.keymaps.local_leader must not be empty",
                });
            }
            if sequence.len() > 64 || tokens.len() > 4 {
                return Err(match name {
                    "input.keymaps.leader" => {
                        "input.keymaps.leader must contain at most 4 keys and 64 bytes"
                    }
                    _ => "input.keymaps.local_leader must contain at most 4 keys and 64 bytes",
                });
            }
            if tokens
                .iter()
                .any(|token| matches!(*token, "<leader>" | "<localleader>"))
            {
                return Err(match name {
                    "input.keymaps.leader" => {
                        "input.keymaps.leader must be a literal sequence without leader tokens"
                    }
                    _ => {
                        "input.keymaps.local_leader must be a literal sequence without leader tokens"
                    }
                });
            }
            if sequence.chars().any(char::is_control) {
                return Err(match name {
                    "input.keymaps.leader" => {
                        "input.keymaps.leader must not contain control characters"
                    }
                    _ => "input.keymaps.local_leader must not contain control characters",
                });
            }
        }
        if self
            .input
            .disabled_command_aliases
            .iter()
            .any(|alias| alias.is_empty() || alias.chars().any(char::is_whitespace))
        {
            return Err("input.disabled_command_aliases contains an invalid alias");
        }
        // This ceiling is a correctness/resource invariant, not product policy.
        if !(1..=1_000_000).contains(&self.input.count_limit) {
            return Err("input.count_limit must be between 1 and 1000000");
        }
        if !(10..=10_000).contains(&self.input.mapping_timeout_ms) {
            return Err("input.mapping_timeout_ms must be between 10 and 10000");
        }
        if !(20..=240).contains(&self.ui.command_line.width) {
            return Err("ui.command_line.width must be between 20 and 240");
        }
        if self.ui.command_line.top_rows > 100 {
            return Err("ui.command_line.top_rows must not exceed 100");
        }
        if self.ui.command_line.minimum_width > self.ui.command_line.width {
            return Err("ui.command_line.minimum_width must not exceed width");
        }
        if self.ui.command_line.outer_margin > 20 {
            return Err("ui.command_line.outer_margin must not exceed 20");
        }
        if !(1..=1_000).contains(&self.ui.command_line.completion_max_rows)
            || self.ui.command_line.input_padding > 20
            || self.ui.command_line.attachment_gap > 20
            || self.ui.command_line.completion_detail_gap > 40
        {
            return Err("ui.command_line layout values exceed safe bounds");
        }
        if !(1..=400).contains(&self.ui.panes.resize_step) {
            return Err("ui.panes.resize_step must be between 1 and 400");
        }
        if !(30..=100).contains(&self.ui.tool_manager.width_percent)
            || !(30..=100).contains(&self.ui.tool_manager.height_percent)
            || self.ui.tool_manager.minimum_width < 20
            || self.ui.tool_manager.minimum_height < 8
            || self.ui.tool_manager.category_order.is_empty()
            || self
                .ui
                .tool_manager
                .category_order
                .iter()
                .any(|category| !self.ui.tool_manager.categories.contains_key(category))
        {
            return Err("ui.tool_manager layout or category policy is invalid");
        }
        let explorer_glyph_width = |glyph: &str| match self.editor.ambiguous_width {
            AmbiguousWidth::Narrow => UnicodeWidthStr::width(glyph),
            AmbiguousWidth::Wide => UnicodeWidthStr::width_cjk(glyph),
        };
        let explorer_glyphs = [
            &self.ui.explorer.glyphs.nerd_font,
            &self.ui.explorer.glyphs.unicode,
            &self.ui.explorer.glyphs.ascii,
        ];
        let invalid_explorer_glyph = explorer_glyphs.into_iter().any(|glyphs| {
            let invalid_bounded = [
                &glyphs.tree_vertical,
                &glyphs.tree_middle,
                &glyphs.tree_last,
                &glyphs.tree_space,
                &glyphs.directory,
                &glyphs.directory_open,
                &glyphs.file,
                &glyphs.symlink,
                &glyphs.loading,
                &glyphs.icon_spacing,
            ]
            .into_iter()
            .any(|glyph| {
                let width = explorer_glyph_width(glyph);
                glyph.is_empty() || width == 0 || width > 8 || glyph.chars().any(char::is_control)
            });
            let invalid_panel = [&glyphs.panel_vertical, &glyphs.panel_horizontal]
                .into_iter()
                .any(|glyph| {
                    UnicodeSegmentation::graphemes(glyph.as_str(), true).count() != 1
                        || explorer_glyph_width(glyph) != 1
                        || glyph.chars().any(char::is_control)
                });
            invalid_bounded || invalid_panel
        });
        if self.ui.explorer.minimum_width < 12
            || self.ui.explorer.maximum_width > 240
            || self.ui.explorer.minimum_width > self.ui.explorer.width
            || self.ui.explorer.width > self.ui.explorer.maximum_width
            || !(1..=10_000).contains(&self.ui.explorer.maximum_visible_rows)
            || !(1..=16 * 1024 * 1024).contains(&self.ui.explorer.maximum_preview_bytes)
            || !(1..=1000).contains(&self.ui.explorer.maximum_preview_lines)
            || self.ui.explorer.focus_marker.is_empty()
            || self.ui.explorer.focus_marker.chars().count() > 8
            || self.ui.explorer.focus_marker.chars().any(char::is_control)
            || self.ui.explorer.loading_label.is_empty()
            || self.ui.explorer.loading_label.chars().count() > 64
            || self.ui.explorer.loading_label.chars().any(char::is_control)
            || invalid_explorer_glyph
            || !(50..=60_000).contains(&self.ui.explorer.watch.poll_interval_ms)
            || self.ui.explorer.watch.debounce_ms
                > self.ui.explorer.watch.poll_interval_ms.saturating_mul(10)
            || !(1..=1024).contains(&self.ui.explorer.watch.maximum_directories)
            || !(1..=100_000).contains(&self.ui.explorer.watch.maximum_entries_per_directory)
            || !(1..=10_000).contains(&self.ui.explorer.watch.maximum_events)
            || !(256..=4 * 1024 * 1024).contains(&self.ui.explorer.watch.maximum_path_bytes)
            || !(1..=1024).contains(&self.ui.explorer.watch.queue_capacity)
        {
            return Err("ui.explorer layout or glyph policy is invalid");
        }
        if !(20..=240).contains(&self.ui.documentation_popup.max_width)
            || !(3..=100).contains(&self.ui.documentation_popup.max_height)
            || self.ui.documentation_popup.horizontal_padding > 8
            || !(1..=100).contains(&self.ui.documentation_popup.scroll_step)
            || !(1..=32).contains(&self.ui.documentation_popup.maximum_sections)
            || !(1..=4096).contains(&self.ui.documentation_popup.maximum_lines)
            || !(256..=1024 * 1024).contains(&self.ui.documentation_popup.maximum_bytes)
        {
            return Err("ui.documentation_popup layout policy is invalid");
        }
        let terminal_panel = &self.ui.terminal_panel;
        if terminal_panel.minimum_height < 3
            || terminal_panel.minimum_height > terminal_panel.height
            || terminal_panel.height > terminal_panel.maximum_height
            || terminal_panel.maximum_height > 200
            || !(1..=100).contains(&terminal_panel.minimum_workspace_height)
            || !(1..=50).contains(&terminal_panel.resize_step)
            || terminal_panel.divider.graphemes(true).count() != 1
            || UnicodeWidthStr::width(terminal_panel.divider.as_str()) != 1
            || terminal_panel.divider.chars().any(char::is_control)
            || [
                &terminal_panel.tab_separator,
                &terminal_panel.normal_mode_label,
                &terminal_panel.visual_mode_label,
            ]
            .into_iter()
            .any(|value| {
                value.is_empty()
                    || value.len() > 256
                    || value.chars().any(char::is_control)
                    || UnicodeWidthStr::width(value.as_str()) > 160
            })
        {
            return Err("ui.terminal_panel layout or presentation policy is invalid");
        }
        if self.ui.key_hints.delay_ms > 60_000
            || !(1..=100).contains(&self.ui.key_hints.max_rows)
            || !(1..=12).contains(&self.ui.key_hints.max_columns)
            || !(12..=240).contains(&self.ui.key_hints.max_width)
            || self.ui.key_hints.horizontal_padding > 8
            || self.ui.key_hints.vertical_padding > 4
            || self.ui.key_hints.row_gap > 4
            || self.ui.key_hints.column_gap > 16
            || self.ui.key_hints.fallback_label.is_empty()
            || self.ui.key_hints.fallback_label.len() > 128
            || self
                .ui
                .key_hints
                .fallback_label
                .chars()
                .any(char::is_control)
            || UnicodeWidthStr::width(self.ui.key_hints.fallback_label.as_str()) > 64
        {
            return Err("ui.key_hints layout or presentation policy is invalid");
        }
        if !(30..=100).contains(&self.ui.picker.width_percent)
            || !(30..=100).contains(&self.ui.picker.height_percent)
            || self.ui.picker.minimum_width < 20
            || self.ui.picker.minimum_height < 6
            || self.ui.picker.selected_marker.chars().any(char::is_control)
            || self.ui.picker.row_marker.chars().any(char::is_control)
            || !(128 * 1024..=8 * 1024 * 1024).contains(&self.ui.picker.maximum_preview_bytes)
        {
            return Err("ui.picker layout or presentation policy is invalid");
        }
        if !(1..=100_000).contains(&self.workspace.maximum_diagnostics)
            || !(1..=100_000).contains(&self.workspace.maximum_diagnostics_per_owner_uri)
            || !(1..=100_000).contains(&self.workspace.maximum_picker_items)
            || !(1..=1_024).contains(&self.workspace.trust_state_queue_capacity)
        {
            return Err("workspace service limits are outside safe bounds");
        }
        if self.schema_version > super::CONFIG_SCHEMA_VERSION {
            return Err("configuration schema is newer than this Dust build");
        }
        if !(1..=4096).contains(&self.debug.maximum_arguments)
            || !(1..=4096).contains(&self.debug.maximum_environment)
            || !(1..=100_000).contains(&self.debug.maximum_breakpoints)
            || !(1..=10_000).contains(&self.debug.maximum_watches)
            || self.debug.adapters.iter().any(|(id, adapter)| {
                id.is_empty()
                    || !adapter.program.is_absolute()
                    || adapter.arguments.len() > self.debug.maximum_arguments
                    || adapter.environment.len() > self.debug.maximum_environment
            })
            || self.debug.sessions.iter().any(|(id, session)| {
                id.is_empty()
                    || !self.debug.adapters.contains_key(&session.adapter)
                    || session.cwd.as_ref().is_some_and(|path| {
                        path.components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))
                    })
                    || session.program.as_ref().is_some_and(|path| {
                        path.components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))
                    })
                    || session
                        .run_in_terminal_profile
                        .as_ref()
                        .is_some_and(|profile| !self.terminals.profiles.contains_key(profile))
                    || session.arguments.len() > self.debug.maximum_arguments
                    || session.path_maps.len() > 1024
            })
        {
            return Err("debug policy is invalid");
        }
        if !(1..=4096).contains(&self.terminals.queue_capacity)
            || !(1..=256 * 1024 * 1024).contains(&self.terminals.maximum_output_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.terminals.maximum_input_bytes)
            || self.terminals.scrollback_bytes > self.terminals.maximum_output_bytes
            || self.terminals.default_profile.is_empty()
            || !self
                .terminals
                .profiles
                .contains_key(&self.terminals.default_profile)
            || self.terminals.profiles.iter().any(|(name, profile)| {
                name.is_empty()
                    || !profile.program.is_absolute()
                    || profile.arguments.len() > 4096
                    || !(1..=1000).contains(&profile.columns)
                    || !(1..=1000).contains(&profile.rows)
            })
        {
            return Err("terminal policy is invalid");
        }
        if (!self.agents.default.is_empty() && !self.agents.profiles.contains_key(&self.agents.default))
            || !(10..=90).contains(&self.agents.panel_width)
            || self.agents.profiles.iter().any(|(name, profile)| {
                name.is_empty()
                    || profile.program.as_os_str().is_empty()
                    || profile.arguments.len() > 4096
            })
        {
            return Err("agent policy is invalid");
        }
        if self
            .plugins
            .store
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
            || !(1..=64).contains(&self.plugins.queue_capacity)
            || !(1024..=256 * 1024 * 1024).contains(&self.plugins.maximum_component_bytes)
            || !(1..=4096).contains(&self.plugins.maximum_commands)
            || self.plugins.fuel == 0
            || self.plugins.entries.iter().any(|(id, entry)| {
                id.is_empty()
                    || entry.manifest.is_absolute()
                    || entry.component.is_absolute()
                    || entry
                        .manifest
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
                    || entry
                        .component
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
            })
        {
            return Err("plugin policy is invalid");
        }
        if self.observability.file_name.is_empty()
            || self.observability.file_name.contains('/')
            || !(1..=100_000).contains(&self.observability.maximum_records)
            || !(1..=64 * 1024).contains(&self.observability.maximum_field_bytes)
            || !(1024..=1024 * 1024 * 1024).contains(&self.observability.maximum_file_bytes)
            || !(1..=32).contains(&self.observability.rotations)
            || !(1..=4096).contains(&self.observability.queue_capacity)
        {
            return Err("observability policy is invalid");
        }
        if !(1..=4096).contains(&self.processes.event_capacity)
            || !(1..=256 * 1024 * 1024).contains(&self.processes.maximum_output_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.processes.maximum_line_bytes)
            || !(1..=256 * 1024 * 1024).contains(&self.processes.maximum_stdin_bytes)
            || !(1..=86_400_000).contains(&self.processes.maximum_timeout_ms)
            || !(1..=256).contains(&self.processes.maximum_running)
            || self.tasks.iter().any(|(name, task)| {
                name.is_empty()
                    || !task.executable.is_absolute()
                    || task.arguments.len() > 4096
                    || task.environment.len() > 4096
                    || task
                        .dependencies
                        .iter()
                        .any(|dependency| !self.tasks.contains_key(dependency))
                    || task
                        .matcher
                        .as_ref()
                        .is_some_and(|matcher| regex::Regex::new(&matcher.pattern).is_err())
                    || matches!(task.kind, TaskKind::Formatter) && !task.stdin
                    || task.timeout_ms.is_some_and(|timeout| {
                        timeout == 0 || timeout > self.processes.maximum_timeout_ms
                    })
            })
            || task_graph_has_cycle(&self.tasks)
        {
            return Err("process or task policy is invalid");
        }
        if self.persistence.state_file.is_empty()
            || self.persistence.state_file.contains('/')
            || self.persistence.state_file.contains('\\')
            || !(1..=64 * 1024 * 1024).contains(&self.persistence.maximum_file_bytes)
            || !(1..=100_000).contains(&self.persistence.maximum_entries)
            || !(1..=64).contains(&self.persistence.queue_capacity)
            || [
                self.persistence.recent_files,
                self.persistence.marks,
                self.persistence.jumps,
                self.persistence.changes,
            ]
            .iter()
            .any(|limit| *limit > self.persistence.maximum_entries)
        {
            return Err("persistence policy is invalid or outside safe bounds");
        }
        if !(1..=1_073_741_824).contains(&self.editor.large_file.byte_threshold)
            || !(1..=10_000_000).contains(&self.editor.large_file.line_threshold)
            || self.editor.large_file.status_label.is_empty()
        {
            return Err("editor.large_file policy is invalid or outside safe bounds");
        }
        if !(1..=1024).contains(&self.syntax.queue_capacity)
            || self.syntax.debounce_ms > 60_000
            || !(1..=268_435_456).contains(&self.syntax.maximum_source_bytes)
            || !(1..=1_000_000).contains(&self.syntax.maximum_spans)
            || !(1..=100_000).contains(&self.syntax.maximum_folds)
            || !(1..=1_000_000).contains(&self.syntax.maximum_indent_rows)
        {
            return Err("syntax policy is invalid or outside safe bounds");
        }
        let workspace_search = &self.workspace.search;
        if !(1..=1_000_000).contains(&workspace_search.maximum_files)
            || !(1..=100_000).contains(&workspace_search.maximum_results)
            || !(1..=268_435_456).contains(&workspace_search.maximum_file_bytes)
            || !(1..=4_294_967_296).contains(&workspace_search.maximum_total_bytes)
            || !(1..=65_536).contains(&workspace_search.maximum_preview_bytes)
            || !(1..=4_096).contains(&workspace_search.batch_size)
            || !(1..=1_024).contains(&workspace_search.event_queue_capacity)
            || workspace_search
                .include_globs
                .iter()
                .chain(&workspace_search.exclude_globs)
                .any(|pattern| globset::Glob::new(pattern).is_err())
        {
            return Err("workspace.search policy is invalid or outside safe bounds");
        }
        if !(100..=900).contains(&self.ui.panes.default_ratio) {
            return Err("ui.panes.default_ratio must be between 100 and 900");
        }
        if self.ui.panes.minimum_width < 3 || self.ui.panes.minimum_height < 2 {
            return Err("ui.panes minimum dimensions are too small");
        }
        if !self.ui.buffer_line.active_format.contains("{title}")
            || !self.ui.buffer_line.active_format.contains("{dirty}")
            || !self.ui.buffer_line.inactive_format.contains("{title}")
            || !self.ui.buffer_line.inactive_format.contains("{dirty}")
            || !(4..=256).contains(&self.ui.buffer_line.maximum_name_width)
            || [
                &self.ui.buffer_line.active_format,
                &self.ui.buffer_line.inactive_format,
                &self.ui.buffer_line.separator,
                &self.ui.buffer_line.dirty_marker,
                &self.ui.buffer_line.scratch_label,
                &self.ui.buffer_line.overflow_left,
                &self.ui.buffer_line.overflow_right,
                &self.ui.buffer_line.pin_marker,
                &self.ui.buffer_line.close_marker,
                &self.ui.buffer_line.diagnostic_error_marker,
                &self.ui.buffer_line.diagnostic_warning_marker,
                &self.ui.buffer_line.scratch_icon,
                &self.ui.buffer_line.virtual_icon,
                &self.ui.buffer_line.terminal_icon,
                &self.ui.buffer_line.custom_icon,
                &self.ui.buffer_line.agent_icon,
            ]
            .iter()
            .any(|value| value.len() > 256 || value.chars().any(char::is_control))
            || [
                &self.ui.buffer_line.pin_marker,
                &self.ui.buffer_line.close_marker,
                &self.ui.buffer_line.diagnostic_error_marker,
                &self.ui.buffer_line.diagnostic_warning_marker,
                &self.ui.buffer_line.scratch_icon,
                &self.ui.buffer_line.virtual_icon,
                &self.ui.buffer_line.terminal_icon,
                &self.ui.buffer_line.custom_icon,
                &self.ui.buffer_line.agent_icon,
            ]
            .iter()
            .any(|glyph| UnicodeWidthStr::width(glyph.as_str()) != 1)
            || self.ui.buffer_line.extension_icons.len() > 64
            || self
                .ui
                .buffer_line
                .extension_icons
                .iter()
                .any(|(extension, glyph)| {
                    extension.is_empty()
                        || extension.len() > 16
                        || !extension
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                        || UnicodeWidthStr::width(glyph.as_str()) != 1
                        || glyph.chars().any(char::is_control)
                })
        {
            return Err("ui.buffer_line policy is invalid");
        }
        if !self.ui.tab_line.active_format.contains("{title}")
            || !self.ui.tab_line.inactive_format.contains("{title}")
        {
            return Err("ui.tab_line formats must contain {title}");
        }
        if self.ui.notifications.timeout_ms > 86_400_000 {
            return Err("ui.notifications.timeout_ms exceeds 24 hours");
        }
        if !(1..=16).contains(&self.ui.lsp_progress.max_rows)
            || !(16..=240).contains(&self.ui.lsp_progress.max_width)
            || self.ui.lsp_progress.right_margin > 40
            || self.ui.lsp_progress.rows_from_bottom > 40
            || !(2..=32).contains(&self.ui.lsp_progress.spinner_frames.len())
            || !(16..=1_000).contains(&self.ui.lsp_progress.spinner_interval_ms)
            || !(100..=60_000).contains(&self.ui.lsp_progress.completed_timeout_ms)
            || self.ui.lsp_progress.spinner_frames.iter().any(|frame| {
                frame.graphemes(true).count() != 1
                    || UnicodeWidthStr::width(frame.as_str()) != 1
                    || frame.chars().any(char::is_control)
            })
            || self
                .ui
                .lsp_progress
                .completed_symbol
                .graphemes(true)
                .count()
                != 1
            || UnicodeWidthStr::width(self.ui.lsp_progress.completed_symbol.as_str()) != 1
            || self
                .ui
                .lsp_progress
                .completed_symbol
                .chars()
                .any(char::is_control)
        {
            return Err("ui.lsp_progress layout or presentation policy is invalid");
        }
        if self.ui.theme.is_empty()
            || !self.ui.theme.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err("ui.theme must be a safe lowercase theme identifier");
        }
        if self
            .editor
            .line_number_separator
            .chars()
            .any(char::is_control)
            || self.editor.line_number_separator.len() > 16
            || self.editor.gutter_padding > 20
            || self.editor.control_character_replacement.chars().count() != 1
            || self.editor.zero_width_replacement.chars().count() != 1
            || self
                .editor
                .control_character_replacement
                .chars()
                .any(char::is_control)
            || self
                .editor
                .zero_width_replacement
                .chars()
                .any(char::is_control)
        {
            return Err("editor gutter presentation is invalid");
        }
        if [
            &self.editor.git_signs.unchanged,
            &self.editor.git_signs.added,
            &self.editor.git_signs.changed,
            &self.editor.git_signs.deleted,
            &self.editor.git_signs.staged,
            &self.editor.debug_signs.breakpoint,
            &self.editor.debug_signs.execution,
        ]
        .into_iter()
        .any(|symbol| {
            symbol.chars().any(char::is_control) || UnicodeWidthStr::width(symbol.as_str()) != 1
        }) {
            return Err("editor.git_signs symbols must each occupy one narrow display cell");
        }
        if self
            .editor
            .indent_guides
            .character
            .chars()
            .any(char::is_control)
            || UnicodeWidthStr::width(self.editor.indent_guides.character.as_str()) != 1
        {
            return Err("editor.indent_guides.character must occupy one narrow display cell");
        }
        // A comment string is a line prefix like `//`, or a template like `<!-- %s -->`.
        if self.editor.comments.values().any(|token| {
            let parts = token.split("%s").collect::<Vec<_>>();
            token.trim().is_empty()
                || token.len() > 24
                || token.chars().any(char::is_control)
                || parts.len() > 2
                || parts[0].trim().is_empty()
                || (parts.len() == 1 && token.chars().any(char::is_whitespace))
        }) {
            return Err(
                "editor.comments values must be a prefix without spaces or one `before %s after` template",
            );
        }
        if self.editor.viewport.horizontal_margin > 100 {
            return Err("editor.viewport.horizontal_margin must not exceed 100");
        }
        if self.editor.viewport.vertical_margin > 100 {
            return Err("editor.viewport.vertical_margin must not exceed 100");
        }
        if !(1..=100).contains(&self.editor.half_page_percent)
            || !(1..=200).contains(&self.editor.full_page_percent)
        {
            return Err("editor scroll percentages are outside safe bounds");
        }
        if !(100..=60_000).contains(&self.lsp.request_timeout_ms)
            || self.lsp.restart_limit > 10
            || !(10..=60_000).contains(&self.lsp.restart_backoff_ms)
            || !(10..=10_000).contains(&self.lsp.maximum_log_lines)
        {
            return Err("lsp policy is outside supported safety bounds");
        }
        if !(1..=1_000).contains(&self.runtime.event_poll_ms) {
            return Err("runtime.event_poll_ms must be between 1 and 1000");
        }
        if !(1..=1024).contains(&self.runtime.vfs_queue_capacity) {
            return Err("runtime.vfs_queue_capacity must be between 1 and 1024");
        }
        if !(1..=64).contains(&self.runtime.macro_recursion_limit)
            || !(1..=100_000).contains(&self.runtime.macro_step_limit)
            || !(1..=100_000).contains(&self.runtime.repeat_step_limit)
        {
            return Err("runtime macro limits exceed safe ceilings");
        }
        if !(25..=60_000).contains(&self.config_watch.poll_interval_ms) {
            return Err("config_watch.poll_interval_ms must be between 25 and 60000");
        }
        if self.config_watch.debounce_ms > 60_000 {
            return Err("config_watch.debounce_ms exceeds 60 seconds");
        }
        if self.recovery.maximum_discovered_records > 10_000 {
            return Err("recovery.maximum_discovered_records exceeds safe ceiling");
        }
        if self.recovery.maximum_retained_records > 10_000 {
            return Err("recovery.maximum_retained_records exceeds safe ceiling");
        }
        if self.recovery.maximum_age_days > 36_500 {
            return Err("recovery.maximum_age_days exceeds 100 years");
        }
        if self.recovery.write_debounce_ms > 60_000 {
            return Err("recovery.write_debounce_ms exceeds 60 seconds");
        }
        if self.history.command.maximum_entries > 100_000
            || self.history.search.maximum_entries > 100_000
        {
            return Err("history maximum_entries exceeds safe ceiling");
        }
        let registry = &self.packages.registry;
        if registry.base_url != "https://dust.blurskye.dev/registry/v1"
            || registry.redirect_limit != 0
            || !(100..=30_000).contains(&registry.connect_timeout_ms)
            || !(100..=120_000).contains(&registry.request_timeout_ms)
            || !(1024..=8 * 1024 * 1024).contains(&registry.maximum_metadata_bytes)
            || !(1024..=256 * 1024 * 1024).contains(&registry.maximum_artifact_bytes)
            || registry.maximum_cache_bytes < registry.maximum_artifact_bytes as u64
            || registry.maximum_cache_bytes > 16 * 1024 * 1024 * 1024
            || !(1..=100_000).contains(&registry.maximum_cache_entries)
            || !(1..=1000).contains(&registry.maximum_search_results)
            || !(1..=64).contains(&registry.queue_capacity)
        {
            return Err("official package registry policy is outside source-owned safety bounds");
        }
        if !(1..=100_000).contains(&self.runtime.completion_limit) {
            return Err("runtime.completion_limit is outside safe bounds");
        }
        const REQUIRED_MESSAGES: &[&str] = &[
            "unsaved_quit",
            "new_scratch",
            "macro_recording",
            "macro_recorded",
            "no_macro_recording",
            "config_reload_queued",
            "unknown_config_key",
            "no_keymap_binding",
            "no_keymap_conflicts",
            "healthy",
            "no_previous_change",
            "no_previous_macro",
            "repeat_step_limit",
            "empty_macro",
            "macro_recursion_limit",
            "macro_step_limit",
            "cursor_not_word",
            "opening_file",
            "no_active_reload",
            "dirty_reload",
            "scratch_reload",
            "reloading",
            "dirty_close",
            "returned_dashboard",
            "buffer_closed",
            "no_active_save",
            "scratch_no_name",
            "saving",
            "reload_rejected",
            "auto_reload_rejected",
            "auto_reloaded",
            "reloaded_restart",
            "reloaded",
            "reloaded_file",
            "reload_closed",
            "unexpected_read",
            "saved_dirty",
            "saved",
            "saved_others_dirty",
            "unexpected_completion",
            "search_empty",
            "search_no_buffer",
            "no_previous_search",
            "pattern_not_found",
            "config_explanation",
            "keymap_inspection",
            "keymap_context_summary",
            "startup_fallback",
            "recovery_summary",
            "recovery_opened",
            "recovery_none",
            "recovery_discarded",
            "recovery_pruned",
            "tool_install_started",
            "tool_install_succeeded",
            "tool_install_failed",
            "tool_no_installer",
            "lsp_stopped",
            "lsp_install_hint",
            "lsp_failed",
            "lsp_status",
            "lsp_no_server",
            "lsp_no_buffer",
            "lsp_disabled",
            "lsp_restart_disabled",
            "lsp_hover",
            "lsp_health",
            "lsp_unsupported",
            "lsp_request_failed",
            "lsp_completion",
            "lsp_symbols",
            "lsp_code_actions",
            "lsp_no_result",
            "syntax_no_fold",
            "task_stopped",
            "lsp_aux_result",
            "lsp_invalid_uri",
            "lsp_invalid_position",
            "lsp_logs",
            "diagnostic_none",
            "diagnostic_current",
            "workspace_trusted",
            "workspace_untrusted",
            "workspace_trust_trusted",
            "workspace_trust_untrusted",
            "workspace_trust_status",
            "workspace_trust_confirmation",
            "workspace_trust_declined",
            "workspace_trust_not_ready",
            "workspace_trust_saving",
            "workspace_trust_state_failed",
            "workspace_root_operation_busy",
            "workspace_root_loading",
            "workspace_root_stale",
            "workspace_root_added",
            "workspace_root_add_failed",
            "workspace_root_removed",
            "workspace_root_activated",
            "workspace_root_dirty_blocker",
            "workspace_root_service_blocker",
            "workspace_root_status",
            "session_saved",
            "session_disabled",
            "format_on_save_skipped",
            "session_restored",
            "session_load_none",
            "session_load_dirty",
            "history_cleared",
            "large_file_invalid",
            "large_file_changed",
            "large_file_status",
            "task_not_running",
            "task_already_running",
            "task_unknown",
            "task_no_root",
            "task_cwd_outside_root",
            "task_exited",
            "task_failed",
            "task_output",
            "plugin_status",
            "plugin_disabled",
            "previous_crash",
            "crash_location",
            "crash_cleared",
            "terminal_unknown",
            "terminal_outside_root",
            "terminal_title",
            "terminal_list",
            "terminal_missing",
            "terminal_yanked",
            "formatter_invalid_utf8",
            "formatter_stale",
            "formatter_applied",
            "formatter_truncated",
            "linter_matcher_missing",
            "picker_tasks_title",
            "picker_tests_title",
            "picker_output_title",
            "picker_code_lens_title",
            "picker_lsp_sessions_title",
            "explorer_title",
            "workspace_search_started",
            "workspace_search_complete",
            "workspace_search_failed",
            "workspace_search_empty",
            "workspace_search_cancelled",
            "workspace_search_stale_result",
            "workspace_search_title",
            "workspace_search_row",
            "workspace_replace_label",
            "picker_references_title",
            "picker_symbols_title",
            "picker_symbol_kind",
            "picker_code_actions_title",
            "picker_diagnostics_title",
            "picker_buffers_title",
            "picker_commands_title",
            "workspace_edit_lsp_label",
            "workspace_edit_server_label",
            "workspace_edit_busy",
            "workspace_resource_unsupported",
            "workspace_edit_loading",
            "workspace_edit_failed",
            "workspace_edit_included",
            "workspace_edit_excluded",
            "workspace_edit_rejected",
            "workspace_edit_applied",
            "dap_started",
            "dap_stopped",
            "dap_failed",
            "dap_status",
            "dap_no_adapter",
            "dap_no_buffer",
        ];
        if REQUIRED_MESSAGES
            .iter()
            .any(|key| !self.messages.contains_key(*key))
        {
            return Err("messages table is missing a required current-surface message");
        }
        if self
            .messages
            .values()
            .any(|message| message.contains('\u{1b}') || message.len() > 4_096)
        {
            return Err("messages contain unsafe or oversized text");
        }
        if [
            "normal",
            "insert",
            "replace",
            "visual_char",
            "visual_line",
            "visual_block",
            "multi",
            "operator_pending",
            "command_line",
            "prompt",
        ]
        .iter()
        .any(|mode| !self.ui.status_line.mode_labels.contains_key(*mode))
        {
            return Err("ui.status_line.mode_labels is incomplete");
        }
        if self.ui.status_line.components.iter().any(|component| {
            !matches!(
                component.as_str(),
                "mode"
                    | "progress"
                    | "file"
                    | "large_file"
                    | "lsp"
                    | "diagnostics"
                    | "git"
                    | "tasks"
                    | "root"
                    | "position"
                    | "breadcrumbs"
            )
        }) {
            return Err("ui.status_line.components contains an unknown component");
        }
        Ok(())
    }
}
