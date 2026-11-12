use harness_core::AgentEvent;
use harness_ui_spec::Screen;

/// Everything the UI layer can observe from the engine, in one stream.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Agent(AgentEvent),
    ScreenSaved(Screen),
}
