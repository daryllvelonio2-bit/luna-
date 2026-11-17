pub mod builder;
pub mod compiler;
pub mod manager;
pub mod manifest;
pub mod runtime;
pub mod skills;
pub mod storage;
pub mod wasm;

pub use builder::{
    generate_plugin_files, slugify_plugin_id, BuilderPanelWidgetDef, BuilderParamDef,
    BuilderScheduleDef, BuilderToolAction, BuilderToolDef, GeneratedPluginFiles, PluginBuilderSpec,
};
pub use compiler::{compile_typescript, CompileError};
pub use manager::{command_index, PluginManager, PluginToolBridge};
pub use manifest::{
    AutoAgentSpec, PluginAgentEntry, PluginCommandSpec, PluginConfigField, PluginManifest,
    PluginScheduleDecl, PluginSummary, PluginUiPanel, PluginWidget,
};
pub use runtime::{PluginRegistrations, PluginRuntimeHandle, RegisteredPluginTool};
pub use skills::{
    names_match, Skill, SkillLoader, SkillMetadata, SkillToolAdapter, SkillViewTool, SkillsListTool,
};
pub use wasm::{SandboxedPlugin, WasmPluginManifest};
