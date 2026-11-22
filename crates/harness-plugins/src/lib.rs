pub mod builder;
pub mod compiler;
pub mod manager;
pub mod manifest;
pub mod plugin_skill;
pub mod runtime;
pub mod skills;
pub mod storage;
pub mod tools;
pub mod wasm;

pub use builder::{
    generate_plugin_files, slugify_plugin_id, BuilderPanelWidgetDef, BuilderParamDef,
    BuilderScheduleDef, BuilderToolAction, BuilderToolDef, GeneratedPluginFiles, PluginBuilderSpec,
};
pub use compiler::{compile_typescript, CompileError};
pub use manager::{command_index, PluginManager, PluginToolBridge};
pub use manifest::{
    AutoAgentSpec, PluginAgentEntry, PluginCommandSpec, PluginConfigField, PluginManifest,
    PluginScheduleDecl, PluginSummary, PluginToolDescriptor, PluginUiPanel, PluginWidget,
};
pub use plugin_skill::{
    build_plugin_turn_prompt, plugin_skill, PLUGIN_PREFILL, PLUGIN_SKILL_DESCRIPTION,
    PLUGIN_SKILL_INSTRUCTIONS, PLUGIN_SKILL_NAME,
};
pub use runtime::{PluginRegistrations, PluginRuntimeHandle, RegisteredPluginTool};
pub use skills::{
    names_match, resolve_pinned_instructions, Skill, SkillLoader, SkillMetadata, SkillToolAdapter,
    SkillViewTool, SkillsListTool,
};
pub use tools::{
    register_plugin_tools, PluginCreateTool, PluginListTool, PluginReloadTool, PluginRemoveTool,
};
pub use wasm::{SandboxedPlugin, WasmPluginManifest};
