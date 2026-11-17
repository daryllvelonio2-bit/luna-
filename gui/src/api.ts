import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppEventPayload,
  CredentialItem,
  GuiBootstrapPayload,
  GuiProviderInfo,
  GuiSessionItem,
  PluginBuilderSpec,
  PluginSummary,
  ScheduleTask,
  TranscriptMessageDto,
} from "./types";

export const api = {
  bootstrap: () => invoke<GuiBootstrapPayload>("bootstrap_state"),
  listSessions: () => invoke<GuiSessionItem[]>("list_sessions"),
  createSession: (pluginAgentId: string | null) =>
    invoke<GuiSessionItem>("create_session", { pluginAgentId }),
  getTranscript: (sessionId: string) =>
    invoke<TranscriptMessageDto[]>("get_session_transcript", { sessionId }),
  renameSession: (sessionId: string, name: string) =>
    invoke<void>("rename_session", { sessionId, name }),
  forkSession: (sessionId: string) =>
    invoke<GuiSessionItem>("fork_session", { sessionId }),
  deleteSession: (sessionId: string) =>
    invoke<boolean>("delete_session", { sessionId }),
  sendMessage: (sessionId: string, message: string, pluginAgentId: string | null) =>
    invoke<void>("send_message", { sessionId, message, pluginAgentId }),
  interruptTurn: (sessionId: string) =>
    invoke<void>("interrupt_turn", { sessionId }),
  answerApproval: (requestId: string, sessionId: string, choice: "once" | "all_session" | "deny") =>
    invoke<void>("answer_approval", { requestId, sessionId, choice }),
  answerClarify: (
    requestId: string,
    response: { answer?: string | null; answers?: Array<{ question: string; answer: string; choice_index?: number | null }> }
  ) => invoke<void>("answer_clarify", { requestId, response }),
  answerSudo: (requestId: string, password: string | null) =>
    invoke<void>("answer_sudo", { requestId, password }),
  listProvidersAndModels: () => invoke<GuiProviderInfo[]>("list_providers_and_models"),
  switchSessionModel: (sessionId: string, provider: string, model: string) =>
    invoke<void>("switch_session_model", { sessionId, provider, model }),
  listCredentials: (provider: string) =>
    invoke<CredentialItem[]>("list_credentials", { provider }),
  setCredentialPriority: (
    sessionId: string,
    provider: string,
    credId: string,
    scope: "session" | "global" | "clear_session"
  ) => invoke<void>("set_credential_priority", { sessionId, provider, credId, scope }),
  listPlugins: () => invoke<PluginSummary[]>("list_plugins"),
  togglePlugin: (pluginId: string, enabled: boolean) =>
    invoke<PluginSummary>("toggle_plugin", { pluginId, enabled }),
  reloadPlugin: (pluginId: string) =>
    invoke<PluginSummary>("reload_plugin", { pluginId }),
  deletePlugin: (pluginId: string) =>
    invoke<void>("delete_plugin", { pluginId }),
  readPluginSource: (pluginId: string) =>
    invoke<string>("read_plugin_source", { pluginId }),
  createPluginFromBuilder: (spec: PluginBuilderSpec) =>
    invoke<PluginSummary>("create_plugin_from_builder", { spec }),
  updatePluginSettings: (pluginId: string, settings: Record<string, unknown>) =>
    invoke<void>("update_plugin_settings", { pluginId, settings }),
  invokePluginCommand: (pluginId: string, command: string, args: unknown = {}) =>
    invoke<string>("invoke_plugin_command", { pluginId, command, args }),
  listSchedules: () => invoke<ScheduleTask[]>("list_schedules"),
  createSchedule: (args: {
    name: string;
    instruction: string;
    rule: string;
    target?: string;
    sessionId?: string;
  }) => invoke<ScheduleTask>("create_schedule", { args }),
  updateSchedule: (args: {
    id: string;
    action: "pause" | "resume" | "delete" | "update";
    name?: string;
    instruction?: string;
    rule?: string;
  }) => invoke<ScheduleTask | null>("update_schedule", { args }),
  runScheduleNow: (id: string) => invoke<ScheduleTask>("run_schedule_now", { id }),
  searchMemory: (query: string) => invoke<Array<Record<string, unknown>>>("search_memory", { query }),
  listNotes: () => invoke<Array<Record<string, unknown>>>("list_notes"),
  readNote: (id: string) => invoke<Record<string, unknown>>("read_note", { id }),

  onAppEvent: (cb: (ev: AppEventPayload) => void): Promise<UnlistenFn> =>
    listen<AppEventPayload>("luna://event", (e) => cb(e.payload)),
  onSessionsUpdated: (cb: () => void): Promise<UnlistenFn> =>
    listen("luna://sessions-updated", () => cb()),
  onPluginsUpdated: (cb: () => void): Promise<UnlistenFn> =>
    listen("luna://plugins-updated", () => cb()),
  onSchedulesUpdated: (cb: () => void): Promise<UnlistenFn> =>
    listen("luna://schedules-updated", () => cb()),
};
