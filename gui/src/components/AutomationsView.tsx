import { useState } from "react";
import { Pause, Play, Plus, Trash2 } from "lucide-react";
import { api } from "../api";
import type { ScheduleTask } from "../types";

interface AutomationsViewProps {
  schedules: ScheduleTask[];
  onRefresh: () => void;
}

export function AutomationsView({ schedules, onRefresh }: AutomationsViewProps) {
  const [name, setName] = useState("");
  const [rule, setRule] = useState("daily:09:00");
  const [instruction, setInstruction] = useState("");
  const [creating, setCreating] = useState(false);

  const create = async () => {
    if (!name.trim() || !instruction.trim()) return;
    setCreating(true);
    try {
      await api.createSchedule({
        name: name.trim(),
        rule: rule.trim(),
        instruction: instruction.trim(),
        target: "new_session_each_run",
      });
      setName("");
      setInstruction("");
      onRefresh();
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="page-scroll">
      <div className="page-header">
        <div>
          <h1>Scheduled Automations</h1>
          <div style={{ fontSize: 13, color: "var(--text-muted)" }}>
            Recurring and one-shot tasks persisted in SQLite (`~/.luna/schedules.db`). Rules: <code>every:3600</code>, <code>daily:09:00</code>, <code>once:2026-10-01T09:00:00Z</code>, or 5-field cron.
          </div>
        </div>
      </div>

      <div className="surface-card" style={{ marginBottom: 18 }}>
        <strong>New Scheduled Task</strong>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 220px", gap: 8 }}>
          <input
            className="text-input"
            placeholder="Task name (e.g. Morning PR Summary)"
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          <input
            className="text-input"
            placeholder="Rule (daily:09:00)"
            value={rule}
            onChange={(e) => setRule(e.target.value)}
          />
        </div>
        <textarea
          className="area-input"
          rows={2}
          placeholder="Instruction for Luna to execute when triggered..."
          value={instruction}
          onChange={(e) => setInstruction(e.target.value)}
        />
        <div>
          <button
            className="btn-primary"
            disabled={creating || !name.trim() || !instruction.trim()}
            onClick={create}
          >
            <Plus size={14} />
            <span>Create Schedule</span>
          </button>
        </div>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        {schedules.length === 0 ? (
          <div style={{ color: "var(--text-muted)" }}>
            No schedules configured yet.
          </div>
        ) : (
          schedules.map((s) => (
            <div key={s.id} className="surface-card">
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 12 }}>
                <div>
                  <div style={{ fontWeight: 650 }}>
                    {s.name}{" "}
                    <span className={s.paused ? "badge-muted" : "badge-ok"}>
                      {s.paused ? "Paused" : s.rule}
                    </span>
                  </div>
                  <div style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 3 }}>
                    {s.instruction}
                  </div>
                  <div style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 4 }}>
                    Next run: {s.next_run_at ?? "—"} &bull; Last run:{" "}
                    {s.last_run_at ?? "never"} {s.last_status ? `(${s.last_status})` : ""}
                  </div>
                </div>
                <div style={{ display: "flex", gap: 6 }}>
                  <button
                    className="btn-secondary"
                    onClick={async () => {
                      await api.runScheduleNow(s.id);
                      onRefresh();
                    }}
                  >
                    <Play size={13} />
                    <span>Run Now</span>
                  </button>
                  <button
                    className="btn-secondary"
                    onClick={async () => {
                      await api.updateSchedule({
                        id: s.id,
                        action: s.paused ? "resume" : "pause",
                      });
                      onRefresh();
                    }}
                  >
                    {s.paused ? <Play size={13} /> : <Pause size={13} />}
                    <span>{s.paused ? "Resume" : "Pause"}</span>
                  </button>
                  <button
                    className="btn-danger"
                    onClick={async () => {
                      await api.updateSchedule({ id: s.id, action: "delete" });
                      onRefresh();
                    }}
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
              </div>
              {s.history.length > 0 && (
                <details className="card-block" style={{ marginBottom: 0 }}>
                  <summary>Delivery history ({s.history.length})</summary>
                  <pre className="mono-pre">
                    {s.history
                      .map(
                        (h) =>
                          `${h.delivered_at} [${h.status}] (${h.session_id}) ${h.summary}`
                      )
                      .join("\n")}
                  </pre>
                </details>
              )}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
