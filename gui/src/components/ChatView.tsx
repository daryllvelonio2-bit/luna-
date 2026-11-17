import { useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Bot,
  Check,
  ChevronRight,
  Cpu,
  Pause,
  Play,
  Send,
  ShieldAlert,
  Square,
  Terminal,
  Trash2,
  X,
} from "lucide-react";
import { api } from "../api";
import type {
  PluginAgentEntry,
  PluginSummary,
  ScheduleTask,
  TranscriptMessageDto,
} from "../types";

export interface LiveToolItem {
  id: string;
  name: string;
  arguments: unknown;
  diff: string | null;
  output?: string;
  is_error?: boolean;
  duration_ms?: number;
  finished: boolean;
}

export interface LiveSubagentItem {
  task_id: string;
  goal: string;
  activity: string;
  finished: boolean;
  success?: boolean;
  summary?: string;
}

export interface PendingApproval {
  request_id: string;
  session_id: string;
  tool: string;
  target: string | null;
  diff: string | null;
}

export interface PendingClarify {
  request_id: string;
  session_id: string;
  questions: Array<{ question: string; choices: string[] }>;
}

export interface PendingSudo {
  request_id: string;
  session_id: string;
  command: string;
}

interface ChatViewProps {
  sessionId: string | null;
  activeAgent: PluginAgentEntry | null;
  activePlugin: PluginSummary | null;
  transcript: TranscriptMessageDto[];
  streamingThinking: string;
  streamingContent: string;
  liveTools: LiveToolItem[];
  liveSubagents: LiveSubagentItem[];
  isRunning: boolean;
  modelLabel: string;
  schedules: ScheduleTask[];
  pendingApproval: PendingApproval | null;
  pendingClarify: PendingClarify | null;
  pendingSudo: PendingSudo | null;
  onSend: (text: string) => void;
  onInterrupt: () => void;
  onOpenSettings: () => void;
  onClearApproval: () => void;
  onClearClarify: () => void;
  onClearSudo: () => void;
  onRefreshSchedules: () => void;
}

export function ChatView({
  sessionId,
  activeAgent,
  activePlugin,
  transcript,
  streamingThinking,
  streamingContent,
  liveTools,
  liveSubagents,
  isRunning,
  modelLabel,
  schedules,
  pendingApproval,
  pendingClarify,
  pendingSudo,
  onSend,
  onInterrupt,
  onOpenSettings,
  onClearApproval,
  onClearClarify,
  onClearSudo,
  onRefreshSchedules,
}: ChatViewProps) {
  const [input, setInput] = useState("");
  const [clarifyAnswers, setClarifyAnswers] = useState<Record<number, string>>({});
  const [sudoPassword, setSudoPassword] = useState("");
  const [panelExpanded, setPanelExpanded] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [transcript.length, streamingContent, streamingThinking, liveTools.length]);

  const submit = () => {
    const trimmed = input.trim();
    if (!trimmed) return;
    setInput("");
    onSend(trimmed);
  };

  const pluginPanels = activePlugin?.panels ?? [];

  return (
    <div className="chat-container">
      <div className="topbar">
        <div className="topbar-left">
          <Bot size={17} color={activeAgent ? "#6aa9ff" : "#e5b84b"} />
          <div style={{ minWidth: 0 }}>
            <div className="topbar-title">
              {activeAgent ? activeAgent.name : "Luna Core"}
            </div>
            <div className="topbar-sub">
              {activeAgent
                ? activeAgent.description
                : sessionId
                ? `Session ${sessionId}`
                : "General coding & research assistant"}
            </div>
          </div>
        </div>
        <div className="topbar-right">
          {pluginPanels.length > 0 && (
            <button
              className="pill-btn"
              onClick={() => setPanelExpanded((v) => !v)}
            >
              <span>{panelExpanded ? "Hide Panel" : "Show Panel"}</span>
            </button>
          )}
          <button className="pill-btn" onClick={onOpenSettings}>
            <Cpu size={13} />
            <span>{modelLabel}</span>
          </button>
        </div>
      </div>

      {pluginPanels.length > 0 && panelExpanded && (
        <div className="plugin-panel-strip">
          {pluginPanels.map((panel) => (
            <div key={panel.id} style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
                <strong>{panel.title}</strong>
                {panel.description && (
                  <span style={{ fontSize: 12, color: "var(--text-muted)" }}>
                    {panel.description}
                  </span>
                )}
              </div>
              <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
                {panel.widgets.map((w, idx) => {
                  if (w.type === "schedule_list") {
                    return (
                      <div key={idx} style={{ width: "100%", display: "flex", flexDirection: "column", gap: 6 }}>
                        {schedules.length === 0 ? (
                          <div style={{ fontSize: 12, color: "var(--text-muted)" }}>
                            No scheduled tasks yet. Ask the Scheduler agent below to create one (e.g. &ldquo;Remind me every morning at 9am to check open PRs&rdquo;).
                          </div>
                        ) : (
                          schedules.map((task) => (
                            <div
                              key={task.id}
                              style={{
                                display: "flex",
                                alignItems: "center",
                                justifyContent: "space-between",
                                gap: 10,
                                padding: "8px 10px",
                                background: "var(--bg-root)",
                                border: "1px solid var(--border-subtle)",
                                borderRadius: "var(--radius-sm)",
                              }}
                            >
                              <div style={{ minWidth: 0 }}>
                                <div style={{ fontWeight: 550, fontSize: 13 }}>
                                  {task.name}{" "}
                                  <span className={task.paused ? "badge-muted" : "badge-ok"}>
                                    {task.paused ? "Paused" : task.rule}
                                  </span>
                                </div>
                                <div style={{ fontSize: 12, color: "var(--text-muted)" }}>
                                  {task.instruction}
                                </div>
                              </div>
                              <div style={{ display: "flex", gap: 6, flexShrink: 0 }}>
                                <button
                                  className="icon-btn"
                                  title="Run now"
                                  onClick={async () => {
                                    await api.runScheduleNow(task.id);
                                    onRefreshSchedules();
                                  }}
                                >
                                  <Play size={14} />
                                </button>
                                <button
                                  className="icon-btn"
                                  title={task.paused ? "Resume" : "Pause"}
                                  onClick={async () => {
                                    await api.updateSchedule({
                                      id: task.id,
                                      action: task.paused ? "resume" : "pause",
                                    });
                                    onRefreshSchedules();
                                  }}
                                >
                                  {task.paused ? <Play size={14} /> : <Pause size={14} />}
                                </button>
                                <button
                                  className="icon-btn"
                                  title="Delete"
                                  onClick={async () => {
                                    await api.updateSchedule({
                                      id: task.id,
                                      action: "delete",
                                    });
                                    onRefreshSchedules();
                                  }}
                                >
                                  <Trash2 size={14} />
                                </button>
                              </div>
                            </div>
                          ))
                        )}
                      </div>
                    );
                  }
                  if (w.type === "status_card") {
                    return (
                      <div
                        key={idx}
                        className="surface-card"
                        style={{ padding: "10px 14px", minWidth: 160 }}
                      >
                        <div style={{ fontSize: 11, color: "var(--text-muted)" }}>{w.title}</div>
                        <div style={{ fontSize: 16, fontWeight: 650 }}>{w.value}</div>
                        {w.hint && <div style={{ fontSize: 11, color: "var(--text-secondary)" }}>{w.hint}</div>}
                      </div>
                    );
                  }
                  if (w.type === "button" && w.command) {
                    return (
                      <button
                        key={idx}
                        className="btn-secondary"
                        onClick={() =>
                          api.invokePluginCommand(panel.plugin_id, w.command!, w.payload ?? {})
                        }
                      >
                        {w.label ?? "Run"}
                      </button>
                    );
                  }
                  return (
                    <div key={idx} style={{ fontSize: 13, color: "var(--text-secondary)" }}>
                      {w.title && <strong>{w.title}: </strong>}
                      {w.content}
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="transcript-scroll">
        {transcript.length === 0 && !isRunning && !streamingContent && (
          <div className="empty-state">
            <h2>{activeAgent ? activeAgent.name : "Ask Luna anything"}</h2>
            <p>
              {activeAgent
                ? `${activeAgent.description}. This agent has ${activeAgent.registered_tools.length} plugin tool(s) and full access to your Luna workspace.`
                : "Write code, inspect repositories, run automated plugins, or schedule recurring tasks."}
            </p>
          </div>
        )}

        {transcript.map((m, idx) => {
          if (m.role === "user") {
            return (
              <div key={idx} className="msg-row">
                <div className="msg-user">{m.content}</div>
              </div>
            );
          }
          if (m.role === "tool") {
            return (
              <div key={idx} className="msg-row">
                <details className="card-block">
                  <summary>
                    <Terminal size={13} />
                    <span>Tool output ({m.tool_call_id ?? "result"})</span>
                  </summary>
                  <pre className="mono-pre">{m.content}</pre>
                </details>
              </div>
            );
          }
          if (m.role === "assistant") {
            return (
              <div key={idx} className="msg-row">
                <div className="msg-assistant">
                  {m.reasoning && (
                    <details className="card-block">
                      <summary>
                        <ChevronRight size={13} />
                        <span>Thinking</span>
                      </summary>
                      <pre className="mono-pre">{m.reasoning}</pre>
                    </details>
                  )}
                  {m.tool_calls.length > 0 && (
                    <details className="card-block">
                      <summary>
                        <Terminal size={13} />
                        <span>
                          Called {m.tool_calls.map((t) => t.name).join(", ")}
                        </span>
                      </summary>
                      <pre className="mono-pre">
                        {JSON.stringify(m.tool_calls, null, 2)}
                      </pre>
                    </details>
                  )}
                  {m.content && <div className="msg-prose">{m.content}</div>}
                </div>
              </div>
            );
          }
          return null;
        })}

        {(isRunning || streamingThinking || streamingContent || liveTools.length > 0) && (
          <div className="msg-row">
            <div className="msg-assistant">
              {streamingThinking && (
                <details className="card-block" open>
                  <summary>
                    <ChevronRight size={13} />
                    <span>Thinking...</span>
                  </summary>
                  <pre className="mono-pre">{streamingThinking}</pre>
                </details>
              )}

              {liveTools.map((t) => (
                <details key={t.id} className="card-block">
                  <summary>
                    <Terminal size={13} />
                    <span>
                      {t.name}{" "}
                      {t.finished ? (
                        <span className={t.is_error ? "badge-err" : "badge-ok"}>
                          {t.is_error ? "error" : `${t.duration_ms ?? 0}ms`}
                        </span>
                      ) : (
                        <span className="nav-tag">running</span>
                      )}
                    </span>
                  </summary>
                  {t.diff ? (
                    <pre className="mono-pre">{t.diff}</pre>
                  ) : (
                    <pre className="mono-pre">
                      {JSON.stringify(t.arguments, null, 2)}
                    </pre>
                  )}
                  {t.output && <pre className="mono-pre">{t.output}</pre>}
                </details>
              ))}

              {liveSubagents.map((s) => (
                <div key={s.task_id} className="card-block">
                  <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12 }}>
                    <strong>Worker: {s.goal}</strong>
                    <span className={s.finished ? (s.success ? "badge-ok" : "badge-err") : "nav-tag"}>
                      {s.finished ? (s.success ? "done" : "failed") : s.activity}
                    </span>
                  </div>
                  {s.summary && (
                    <div style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 4 }}>
                      {s.summary}
                    </div>
                  )}
                </div>
              ))}

              {streamingContent ? (
                <div className="msg-prose">{streamingContent}</div>
              ) : (
                isRunning && (
                  <div style={{ fontSize: 13, color: "var(--text-muted)" }}>
                    Working...
                  </div>
                )
              )}
            </div>
          </div>
        )}

        {pendingApproval && (
          <div className="approval-card">
            <div style={{ display: "flex", alignItems: "center", gap: 8, fontWeight: 600 }}>
              <ShieldAlert size={16} color="#e5b84b" />
              <span>Approve tool: {pendingApproval.tool}</span>
            </div>
            {pendingApproval.target && (
              <div style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 4 }}>
                Target: <code>{pendingApproval.target}</code>
              </div>
            )}
            {pendingApproval.diff && (
              <pre className="mono-pre">{pendingApproval.diff}</pre>
            )}
            <div className="approval-actions">
              <button
                className="btn-primary"
                onClick={async () => {
                  await api.answerApproval(
                    pendingApproval.request_id,
                    pendingApproval.session_id,
                    "once"
                  );
                  onClearApproval();
                }}
              >
                <Check size={14} />
                <span>Approve once</span>
              </button>
              <button
                className="btn-secondary"
                onClick={async () => {
                  await api.answerApproval(
                    pendingApproval.request_id,
                    pendingApproval.session_id,
                    "all_session"
                  );
                  onClearApproval();
                }}
              >
                <span>Allow all this session</span>
              </button>
              <button
                className="btn-danger"
                onClick={async () => {
                  await api.answerApproval(
                    pendingApproval.request_id,
                    pendingApproval.session_id,
                    "deny"
                  );
                  onClearApproval();
                }}
              >
                <X size={14} />
                <span>Deny</span>
              </button>
            </div>
          </div>
        )}

        {pendingClarify && (
          <div className="approval-card">
            <div style={{ display: "flex", alignItems: "center", gap: 8, fontWeight: 600 }}>
              <AlertTriangle size={16} color="#e5b84b" />
              <span>Luna needs clarification</span>
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: 12, marginTop: 10 }}>
              {pendingClarify.questions.map((q, qi) => (
                <div key={qi} style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                  <div style={{ fontSize: 13 }}>{q.question}</div>
                  {q.choices.length > 0 && (
                    <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                      {q.choices.map((ch) => (
                        <button
                          key={ch}
                          className={
                            clarifyAnswers[qi] === ch ? "btn-primary" : "btn-secondary"
                          }
                          onClick={() =>
                            setClarifyAnswers((prev) => ({ ...prev, [qi]: ch }))
                          }
                        >
                          {ch}
                        </button>
                      ))}
                    </div>
                  )}
                  <input
                    className="text-input"
                    placeholder="Or type a custom answer..."
                    value={clarifyAnswers[qi] ?? ""}
                    onChange={(e) =>
                      setClarifyAnswers((prev) => ({
                        ...prev,
                        [qi]: e.target.value,
                      }))
                    }
                  />
                </div>
              ))}
              <div>
                <button
                  className="btn-primary"
                  onClick={async () => {
                    const answers = pendingClarify.questions.map((q, qi) => ({
                      question: q.question,
                      answer: clarifyAnswers[qi] ?? q.choices[0] ?? "",
                    }));
                    await api.answerClarify(pendingClarify.request_id, {
                      answer: answers[0]?.answer ?? "",
                      answers,
                    });
                    setClarifyAnswers({});
                    onClearClarify();
                  }}
                >
                  Submit Answers
                </button>
              </div>
            </div>
          </div>
        )}

        {pendingSudo && (
          <div className="approval-card">
            <div style={{ fontWeight: 600 }}>Sudo password required</div>
            <div style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 4 }}>
              Command: <code>{pendingSudo.command}</code>
            </div>
            <div style={{ display: "flex", gap: 8, marginTop: 10 }}>
              <input
                type="password"
                className="text-input"
                style={{ flex: 1 }}
                placeholder="Password"
                value={sudoPassword}
                onChange={(e) => setSudoPassword(e.target.value)}
              />
              <button
                className="btn-primary"
                onClick={async () => {
                  await api.answerSudo(pendingSudo.request_id, sudoPassword || null);
                  setSudoPassword("");
                  onClearSudo();
                }}
              >
                Submit
              </button>
              <button
                className="btn-secondary"
                onClick={async () => {
                  await api.answerSudo(pendingSudo.request_id, null);
                  setSudoPassword("");
                  onClearSudo();
                }}
              >
                Cancel
              </button>
            </div>
          </div>
        )}

        <div ref={bottomRef} />
      </div>

      <div className="composer-wrap">
        <div className="composer-box">
          <textarea
            className="composer-textarea"
            rows={1}
            placeholder={
              activeAgent
                ? `Message ${activeAgent.name}...`
                : "Message Luna... (Enter to send, Shift+Enter for newline)"
            }
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                submit();
              }
            }}
          />
          {isRunning ? (
            <button className="btn-danger" onClick={onInterrupt} title="Stop turn">
              <Square size={14} />
              <span>Stop</span>
            </button>
          ) : (
            <button
              className="btn-primary"
              disabled={!input.trim()}
              onClick={submit}
            >
              <Send size={14} />
              <span>Send</span>
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
