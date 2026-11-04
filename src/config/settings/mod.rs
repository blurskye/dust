mod theme;
pub use theme::{
    HighlightDefinition, ThemeAttribute, ThemeColor, ThemeConfig, ThemeStyle, ThemeStyles,
};
mod editor;
pub use editor::{
    AmbiguousWidth, ClipboardSync, ColorMode, CursorShape, EditorConfig, EditorViewportConfig,
    FinalNewlinePolicy, GitSignsConfig, IndentGuideConfig, LargeFileConfig, LineEndingPolicy,
    LineNumbers, RegisterConfig,
};
mod ui;
pub use ui::{
    BorderStyle, BufferLineConfig, CommandLineConfig, CursorConfig, DocumentationPopupConfig,
    ExplorerConfig, ExplorerGlyphSet, ExplorerGlyphs, ExplorerIconMode, KeyHintConfig,
    LspProgressConfig, PromptTextConfig, StatusLineConfig, TerminalPanelConfig, ToolManagerConfig,
    UiConfig,
};
mod input;
pub use input::{InputConfig, KeymapPolicyConfig, StartupConfig};
mod workspace;
pub use workspace::{RecoveryConfig, SearchConfig, WorkspaceConfig, WorkspaceSearchConfig};
mod language;
pub use language::{InlayHintLevel, LspConfig, PersistenceConfig, SyntaxConfig};
mod tasks;
pub use tasks::{ProblemMatcherConfig, TaskConfig, TaskKind};
mod tools;
pub use tools::{PluginEntryConfig, PluginsConfig};
mod debugger;
pub use debugger::{DebugAdapterConfig, DebugConfig, DebugRequestKind, DebugSessionConfig};
mod agents;
pub use agents::{AgentConnection, AgentProfileConfig, AgentsConfig};
mod terminal;
pub use terminal::{TerminalProfileConfig, TerminalsConfig};
mod windows;
pub use windows::{WindowPlacementConfig, WindowRuleConfig, WindowsConfig};
