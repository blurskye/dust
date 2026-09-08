use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn nix_workspace_available_for(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        self.tooling.nix_workspace_available_for(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }

    pub(in crate::editor::session) fn selected_lsp_runtime_source(
        &self,
        definition: &ToolDefinition,
    ) -> LspRuntimeSource {
        self.tooling.selected_lsp_runtime_source(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }

    pub(in crate::editor::session) fn selected_managed_definition(
        &self,
        definition: &ToolDefinition,
    ) -> ToolDefinition {
        self.tooling.selected_managed_definition(definition)
    }

    pub(in crate::editor::session) fn managed_variant_installed(
        &self,
        base: &ToolDefinition,
        variant: &ToolDefinition,
    ) -> bool {
        self.tooling.managed_variant_installed(base, variant)
    }

    pub(in crate::editor::session) fn selected_managed_executable(
        &self,
        definition: &ToolDefinition,
    ) -> Option<PathBuf> {
        self.tooling.selected_managed_executable(definition)
    }

    pub(in crate::editor::session) fn selected_lsp_runtime_available(
        &self,
        definition: &ToolDefinition,
    ) -> bool {
        self.tooling.selected_lsp_runtime_available(
            definition,
            self.configuration.settings().lsp.nix_workspace_fallback,
        )
    }

    pub(in crate::editor::session) fn tool_version_key(
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> String {
        crate::editor::session::tooling::ToolingSession::tool_version_key(
            definition, source, executable,
        )
    }

    pub(in crate::editor::session) fn rust_analyzer_is_rustup_proxy(path: &Path) -> bool {
        crate::editor::session::tooling::ToolingSession::rust_analyzer_is_rustup_proxy(path)
    }

    pub(in crate::editor::session) fn unavailable_rust_analyzer_message() -> String {
        crate::editor::session::tooling::ToolingSession::unavailable_rust_analyzer_message()
    }

    pub(in crate::editor::session) fn rust_analyzer_path_version(
        version: &str,
        rustup_bundled: bool,
    ) -> (String, String) {
        crate::editor::session::tooling::ToolingSession::rust_analyzer_path_version(
            version,
            rustup_bundled,
        )
    }

    pub(in crate::editor::session) fn observed_tool_version(
        &self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: Option<&PathBuf>,
    ) -> (String, Option<String>) {
        self.tooling
            .observed_tool_version(definition, source, executable)
    }

    pub(in crate::editor::session) fn queue_tool_version_probe(
        &mut self,
        definition: &ToolDefinition,
        source: LspRuntimeSource,
        executable: PathBuf,
    ) {
        self.tooling
            .queue_tool_version_probe(definition, source, executable)
    }

    pub(in crate::editor::session) fn queue_tool_version_probes(&mut self, category: &str) {
        self.tooling.queue_tool_version_probes(category)
    }
}
