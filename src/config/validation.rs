//! Configuration value validation and required defaults.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::config::settings::AmbiguousWidth;
use crate::config::settings::TaskConfig;
use crate::config::settings::TaskKind;
use crate::config::settings::WindowPlacementConfig;

fn task_graph_has_cycle(tasks: &BTreeMap<String, TaskConfig>) -> bool {
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for name in tasks.keys() {
        let mut stack = vec![(name.as_str(), false)];
        while let Some((name, leaving)) = stack.pop() {
            if leaving {
                visiting.remove(name);
                visited.insert(name);
            } else if !visited.contains(name) {
                if !visiting.insert(name) {
                    return true;
                }
                stack.push((name, true));
                if let Some(task) = tasks.get(name) {
                    stack.extend(
                        task.dependencies
                            .iter()
                            .rev()
                            .map(|name| (name.as_str(), false)),
                    );
                }
            }
        }
    }
    false
}

/// What is wrong with one configured task, as the end of "tasks.NAME ...".
fn task_problem(task: &TaskConfig, tasks: &BTreeMap<String, TaskConfig>) -> Option<String> {
    if !task.executable.is_absolute() {
        return Some(format!(
            "executable must be an absolute path, such as /usr/bin/{}",
            task.executable.display()
        ));
    }
    if task.arguments.len() > 4096 || task.environment.len() > 4096 {
        return Some("has more than 4096 arguments or environment variables".into());
    }
    if let Some(missing) = task
        .dependencies
        .iter()
        .find(|dependency| !tasks.contains_key(*dependency))
    {
        return Some(format!("depends on {missing}, which is not a task"));
    }
    if task
        .matcher
        .as_ref()
        .is_some_and(|matcher| regex::Regex::new(&matcher.pattern).is_err())
    {
        return Some("matcher.pattern is not a valid regular expression".into());
    }
    if matches!(task.kind, TaskKind::Formatter) && !task.stdin {
        return Some("is a formatter, so it needs stdin = true".into());
    }
    if task
        .timeout_ms
        .is_some_and(|timeout| timeout == 0 || timeout > 86_400_000)
    {
        return Some("timeout_ms must be between 1 and 86400000".into());
    }
    None
}

impl Config {
    pub(crate) fn validate(&self) -> Result<(), String> {
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
            return Err("windows.rule exceeds the 32-rule limit".into());
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
                return Err("windows.rule contains an unsupported content_type".into());
            }
            if !window_targets.insert(rule.content_type.as_str()) {
                return Err("windows.rule contains a duplicate content_type".into());
            }
            match &rule.placement {
                WindowPlacementConfig::Automatic | WindowPlacementConfig::Hidden => {}
                WindowPlacementConfig::Docked { edge, size } => {
                    if !matches!(edge.as_str(), "left" | "right" | "top" | "bottom")
                        || !(1..=240).contains(size)
                    {
                        return Err("windows.rule contains an invalid dock placement".into());
                    }
                }
                WindowPlacementConfig::Floating { width, height, .. } => {
                    if *width == 0 || *height == 0 {
                        return Err("windows.rule contains an invalid floating placement".into());
                    }
                }
                WindowPlacementConfig::Fullscreen => fullscreen_rules += 1,
            }
        }
        if fullscreen_rules > 1 {
            return Err("windows.rule contains conflicting fullscreen defaults".into());
        }
        if !(1..=32).contains(&self.editor.tab_width) {
            return Err("editor.tab_width must be between 1 and 32".into());
        }
        if !(1..=32).contains(&self.editor.indent_width) {
            return Err("editor.indent_width must be between 1 and 32".into());
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
                return Err(format!("{name} must not be empty"));
            }
            if sequence.len() > 64 || tokens.len() > 4 {
                return Err(format!("{name} must contain at most 4 keys and 64 bytes"));
            }
            if tokens
                .iter()
                .any(|token| matches!(*token, "<leader>" | "<localleader>"))
            {
                return Err(format!(
                    "{name} must be a literal sequence without leader tokens"
                ));
            }
            if sequence.chars().any(char::is_control) {
                return Err(format!("{name} must not contain control characters"));
            }
        }
        if !(10..=10_000).contains(&self.input.mapping_timeout_ms) {
            return Err("input.mapping_timeout_ms must be between 10 and 10000".into());
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
            [
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
            })
        });
        if !(12..=240).contains(&self.ui.explorer.width) || invalid_explorer_glyph {
            return Err("ui.explorer width or glyph policy is invalid".into());
        }
        if !(3..=200).contains(&self.ui.terminal_panel.height) {
            return Err("ui.terminal_panel.height must be between 3 and 200".into());
        }
        if self.ui.key_hints.delay_ms > 60_000 {
            return Err("ui.key_hints.delay_ms exceeds 60 seconds".into());
        }
        if self.schema_version > crate::config::CONFIG_SCHEMA_VERSION {
            return Err("configuration schema is newer than this Dust build".into());
        }
        if self.debug.adapters.iter().any(|(id, adapter)| {
            id.is_empty()
                || !adapter.program.is_absolute()
                || adapter.arguments.len() > 4096
                || adapter.environment.len() > 4096
        }) || self.debug.sessions.iter().any(|(id, session)| {
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
                || session.arguments.len() > 4096
                || session.path_maps.len() > 1024
        }) {
            return Err("debug policy is invalid".into());
        }
        if self.terminals.default_profile.is_empty()
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
            return Err("terminal policy is invalid".into());
        }
        if (!self.agents.default.is_empty()
            && !self.agents.profiles.contains_key(&self.agents.default))
            || !(10..=90).contains(&self.agents.panel_width)
            || self.agents.profiles.iter().any(|(name, profile)| {
                name.is_empty()
                    || profile.program.as_os_str().is_empty()
                    || profile.arguments.len() > 4096
            })
        {
            return Err("agent policy is invalid".into());
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
            return Err("plugin policy is invalid".into());
        }
        for (name, task) in &self.tasks {
            if let Some(problem) = task_problem(task, &self.tasks) {
                return Err(format!("tasks.{name} {problem}"));
            }
        }
        if self.tasks.keys().any(String::is_empty) {
            return Err("a task needs a name".into());
        }
        if task_graph_has_cycle(&self.tasks) {
            return Err("tasks depend on each other in a loop".into());
        }
        if self
            .workspace
            .search
            .exclude_globs
            .iter()
            .any(|pattern| globset::Glob::new(pattern).is_err())
        {
            return Err("workspace.search.exclude_globs contains an invalid glob".into());
        }
        if self.ui.buffer_line.extension_icons.len() > 64
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
            return Err("ui.buffer_line.extension_icons is invalid".into());
        }
        if !crate::config::themes::valid_theme_name(&self.ui.theme) {
            return Err("ui.theme must be a safe lowercase theme identifier".into());
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
                "editor.comments values must be a prefix without spaces or one `before %s after` template"
                    .into(),
            );
        }
        if self.editor.viewport.horizontal_margin > 100 {
            return Err("editor.viewport.horizontal_margin must not exceed 100".into());
        }
        if self.editor.viewport.vertical_margin > 100 {
            return Err("editor.viewport.vertical_margin must not exceed 100".into());
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
            "recovery_opened",
            "recovery_restored",
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
            "lsp_loading",
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
            return Err("messages table is missing a required current-surface message".into());
        }
        if self
            .messages
            .values()
            .any(|message| message.contains('\u{1b}') || message.len() > 4_096)
        {
            return Err("messages contain unsafe or oversized text".into());
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
            "terminal",
        ]
        .iter()
        .any(|mode| !self.ui.status_line.mode_labels.contains_key(*mode))
        {
            return Err("ui.status_line.mode_labels is incomplete".into());
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
            return Err("ui.status_line.components contains an unknown component".into());
        }
        Ok(())
    }
}
