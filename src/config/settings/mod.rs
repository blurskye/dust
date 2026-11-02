mod theme;
pub use theme::{HighlightDefinition, ThemeAttribute, ThemeColor, ThemeConfig, ThemeStyle, ThemeStyles};
mod editor;
pub use editor::{
    AmbiguousWidth, BomPolicy, ColorMode, CursorShape, EditorConfig, EditorViewportConfig,
    FinalNewlinePolicy, GitSignsConfig, IndentGuideConfig, LargeFileConfig, LineEndingPolicy,
    LineNumbers, RegisterConfig, ClipboardSync, RegisterStorage,
};
mod ui;
pub use ui::{
    BorderStyle, BufferLineConfig, CommandLineConfig, CursorConfig, DocumentationPopupConfig,
    ExplorerConfig, ExplorerGlyphSet, ExplorerGlyphs, ExplorerIconMode, ExplorerWatchConfig,
    KeyHintAnchor, KeyHintConfig, LspProgressConfig, NotificationConfig, PaneConfig, PickerConfig,
    PromptTextConfig, StatusLineConfig, TabLineConfig, TerminalPanelConfig, ToolManagerConfig,
    UiConfig,
};
mod input;
pub use input::{InputConfig, KeymapPolicyConfig, StartupConfig};
mod workspace;
pub use workspace::{
    ConfigWatchConfig, HistoryConfig, HistoryStreamConfig, RecoveryConfig, RuntimeConfig,
    SearchConfig, WorkspaceConfig, WorkspaceSearchConfig,
};
mod language;
pub use language::{InlayHintLevel, LspConfig, PersistenceConfig, ProcessConfig, SyntaxConfig};
mod tasks;
pub use tasks::{ObservabilityConfig, ProblemMatcherConfig, TaskConfig, TaskKind};
mod tools;
pub use tools::{PackageRegistryConfig, PackagesConfig, PluginEntryConfig, PluginsConfig};
mod debugger;
pub use debugger::{DebugAdapterConfig, DebugConfig, DebugRequestKind, DebugSessionConfig};
mod agents;
pub use agents::{AgentProfileConfig, AgentsConfig};
mod terminal;
pub use terminal::{TerminalProfileConfig, TerminalsConfig};
mod windows;
pub use windows::{WindowPlacementConfig, WindowRuleConfig, WindowsConfig};
