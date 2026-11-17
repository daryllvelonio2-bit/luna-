import React, { useState, useEffect, useRef } from 'react';
import type {
  ApprovalRequestPayload,
  ClarifyRequestPayload,
  TranscriptMessageDto,
} from '../types';
import {
  Send,
  Square,
  ChevronDown,
  ChevronRight,
  ShieldAlert,
  HelpCircle,
  Wrench,
  Sparkles,
} from 'lucide-react';

interface ChatViewProps {
  sessionId: string | null;
  sessionTitle: string;
  provider: string;
  model: string;
  messages: TranscriptMessageDto[];
  turnRunning: boolean;
  onSendMessage: (text: string) => void;
  onInterrupt: () => void;
  pendingApproval: ApprovalRequestPayload | null;
  onResolveApproval: (
    decision: 'run' | 'deny' | 'run_with_args' | 'deny_with_feedback',
    args?: unknown,
    feedback?: string
  ) => void;
  pendingClarify: ClarifyRequestPayload | null;
  onResolveClarify: (answers: Array<string | string[]>) => void;
  onAskApprovalQuestion: (
    toolName: string,
    target: string | null,
    question: string
  ) => Promise<string>;
}

export const ChatView: React.FC<ChatViewProps> = ({
  sessionId,
  sessionTitle,
  provider,
  model,
  messages,
  turnRunning,
  onSendMessage,
  onInterrupt,
  pendingApproval,
  onResolveApproval,
  pendingClarify,
  onResolveClarify,
  onAskApprovalQuestion,
}) => {
  const [input, setInput] = useState('');
  const [expandedReasoning, setExpandedReasoning] = useState<Record<number, boolean>>({});
  const [expandedTools, setExpandedTools] = useState<Record<string, boolean>>({});

  // Approval Q&A state
  const [approvalQuestion, setApprovalQuestion] = useState('');
  const [approvalReplies, setApprovalReplies] = useState<Array<{ q: string; a: string }>>([]);
  const [askingApproval, setAskingApproval] = useState(false);

  // Clarify state
  const [clarifyAnswers, setClarifyAnswers] = useState<Record<number, string>>({});
  const [customClarifyInput, setCustomClarifyInput] = useState('');

  const messagesEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, turnRunning]);

  const handleSend = () => {
    if (!input.trim() || !sessionId) return;
    onSendMessage(input.trim());
    setInput('');
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const toggleReasoning = (idx: number) => {
    setExpandedReasoning((prev) => ({ ...prev, [idx]: !prev[idx] }));
  };

  const toggleTool = (id: string) => {
    setExpandedTools((prev) => ({ ...prev, [id]: !prev[id] }));
  };

  const handleAskApproval = async () => {
    if (!approvalQuestion.trim() || !pendingApproval) return;
    setAskingApproval(true);
    try {
      const q = approvalQuestion.trim();
      const a = await onAskApprovalQuestion(
        pendingApproval.tool_name,
        pendingApproval.target || null,
        q
      );
      setApprovalReplies((prev) => [...prev, { q, a }]);
      setApprovalQuestion('');
    } catch (e) {
      alert(`Error asking model: ${String(e)}`);
    } finally {
      setAskingApproval(false);
    }
  };

  const handleClarifySubmit = () => {
    if (!pendingClarify) return;
    const questions = pendingClarify.questions && pendingClarify.questions.length > 0
      ? pendingClarify.questions
      : [{ question: pendingClarify.question, choices: pendingClarify.choices }];

    const answers = questions.map((_, idx) => {
      return clarifyAnswers[idx] || customClarifyInput || 'Skipped';
    });
    onResolveClarify(answers);
    setClarifyAnswers({});
    setCustomClarifyInput('');
  };

  return (
    <div className="chat-view">
      <div className="chat-header">
        <div className="chat-header-title">
          <span>{sessionTitle || 'Select or create a conversation'}</span>
        </div>
        <div className="chat-header-meta">
          <span className="badge active">
            {provider || 'default'} : {model || 'auto'}
          </span>
          {turnRunning && (
            <span className="badge" style={{ color: 'var(--accent-color)' }}>
              Thinking...
            </span>
          )}
        </div>
      </div>

      <div className="messages-area">
        {messages.map((m, idx) => {
          const isUser = m.role === 'user';
          const isAssistant = m.role === 'assistant';
          const hasReasoning = Boolean(m.reasoning);
          const hasTools = m.tool_calls && m.tool_calls.length > 0;

          return (
            <div
              key={idx}
              className={`message-card ${isUser ? 'user' : isAssistant ? 'assistant' : 'badge'}`}
            >
              {hasReasoning && (
                <div className="reasoning-box">
                  <div
                    className="reasoning-header"
                    onClick={() => toggleReasoning(idx)}
                  >
                    <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                      <Sparkles size={14} color="var(--accent-color)" />
                      <span>Thinking Process</span>
                    </div>
                    {expandedReasoning[idx] ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                  </div>
                  {expandedReasoning[idx] && (
                    <div className="reasoning-content">{m.reasoning}</div>
                  )}
                </div>
              )}

              {m.content && <div style={{ whiteSpace: 'pre-wrap' }}>{m.content}</div>}

              {hasTools && (
                <div className="tool-calls-container">
                  {m.tool_calls.map((tc) => {
                    const expanded = expandedTools[tc.id];
                    return (
                      <div key={tc.id} className="tool-call-card">
                        <div className="tool-call-header" onClick={() => toggleTool(tc.id)}>
                          <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                            <Wrench size={13} color="var(--warning-color)" />
                            <span>{tc.name}</span>
                          </div>
                          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                        </div>
                        {expanded && (
                          <div className="tool-call-body">
                            {JSON.stringify(tc.arguments, null, 2)}
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
        <div ref={messagesEndRef} />
      </div>

      {pendingApproval && (
        <div className="modal-backdrop">
          <div className="modal-card">
            <div className="modal-header">
              <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                <ShieldAlert size={18} color="var(--warning-color)" />
                <span style={{ fontWeight: 600 }}>Tool Call Approval Request</span>
              </div>
              <span className="badge">{pendingApproval.tool_name}</span>
            </div>

            <div className="modal-body">
              {pendingApproval.target && (
                <div className="form-group">
                  <span className="form-label">Target Resource:</span>
                  <div style={{ fontFamily: 'monospace', fontSize: '13px' }}>
                    {pendingApproval.target}
                  </div>
                </div>
              )}

              {pendingApproval.diff ? (
                <div className="form-group">
                  <span className="form-label">Proposed Diff:</span>
                  <div className="diff-view">{pendingApproval.diff}</div>
                </div>
              ) : (
                <div className="form-group">
                  <span className="form-label">Tool Arguments:</span>
                  <div className="diff-view">
                    {JSON.stringify(pendingApproval.arguments, null, 2)}
                  </div>
                </div>
              )}

              {approvalReplies.length > 0 && (
                <div className="form-group">
                  <span className="form-label">Model Suggestions / Q&A:</span>
                  {approvalReplies.map((r, i) => (
                    <div
                      key={i}
                      style={{
                        background: 'var(--code-bg)',
                        border: '1px solid var(--border-color)',
                        borderRadius: '6px',
                        padding: '8px',
                        fontSize: '12px',
                        marginBottom: '6px',
                      }}
                    >
                      <div style={{ fontWeight: 600, color: 'var(--accent-color)' }}>
                        Q: {r.q}
                      </div>
                      <div style={{ marginTop: '4px', whiteSpace: 'pre-wrap' }}>{r.a}</div>
                    </div>
                  ))}
                </div>
              )}

              <div className="form-group">
                <span className="form-label">Ask the model or propose a modification:</span>
                <div style={{ display: 'flex', gap: '8px' }}>
                  <input
                    type="text"
                    className="form-input"
                    style={{ flex: 1 }}
                    placeholder="e.g. Can you update only line 15 or check the imports first?"
                    value={approvalQuestion}
                    onChange={(e) => setApprovalQuestion(e.target.value)}
                    onKeyDown={(e) => e.key === 'Enter' && handleAskApproval()}
                  />
                  <button
                    className="secondary-btn"
                    onClick={handleAskApproval}
                    disabled={askingApproval || !approvalQuestion.trim()}
                  >
                    {askingApproval ? 'Asking...' : 'Ask'}
                  </button>
                </div>
              </div>
            </div>

            <div className="modal-footer">
              <button
                className="danger-btn"
                onClick={() => onResolveApproval('deny')}
              >
                Deny
              </button>
              {approvalReplies.length > 0 && (
                <button
                  className="secondary-btn"
                  onClick={() =>
                    onResolveApproval(
                      'deny_with_feedback',
                      undefined,
                      approvalReplies[approvalReplies.length - 1].q
                    )
                  }
                >
                  Deny with Feedback
                </button>
              )}
              <button
                className="primary-btn"
                style={{ backgroundColor: 'var(--success-color)' }}
                onClick={() => onResolveApproval('run')}
              >
                Approve Once
              </button>
            </div>
          </div>
        </div>
      )}

      {pendingClarify && (
        <div className="modal-backdrop">
          <div className="modal-card">
            <div className="modal-header">
              <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                <HelpCircle size={18} color="var(--accent-color)" />
                <span style={{ fontWeight: 600 }}>Clarification Needed</span>
              </div>
            </div>

            <div className="modal-body">
              {pendingClarify.questions && pendingClarify.questions.length > 0 ? (
                pendingClarify.questions.map((q, idx) => (
                  <div key={idx} className="form-group">
                    <span className="form-label" style={{ fontSize: '13px', color: 'var(--text-primary)' }}>
                      {q.question}
                    </span>
                    <div style={{ display: 'flex', flexDirection: 'column', gap: '6px', marginTop: '6px' }}>
                      {q.choices?.map((choice) => (
                        <label
                          key={choice}
                          style={{
                            display: 'flex',
                            alignItems: 'center',
                            gap: '8px',
                            cursor: 'pointer',
                            fontSize: '13px',
                          }}
                        >
                          <input
                            type="radio"
                            name={`choice-${idx}`}
                            checked={clarifyAnswers[idx] === choice}
                            onChange={() => setClarifyAnswers((prev) => ({ ...prev, [idx]: choice }))}
                          />
                          <span>{choice}</span>
                        </label>
                      ))}
                    </div>
                  </div>
                ))
              ) : (
                <div className="form-group">
                  <span className="form-label" style={{ fontSize: '13px', color: 'var(--text-primary)' }}>
                    {pendingClarify.question}
                  </span>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: '6px', marginTop: '6px' }}>
                    {pendingClarify.choices.map((choice) => (
                      <label
                        key={choice}
                        style={{
                          display: 'flex',
                          alignItems: 'center',
                          gap: '8px',
                          cursor: 'pointer',
                          fontSize: '13px',
                        }}
                      >
                        <input
                          type="radio"
                          name="choice-0"
                          checked={clarifyAnswers[0] === choice}
                          onChange={() => setClarifyAnswers((prev) => ({ ...prev, [0]: choice }))}
                        />
                        <span>{choice}</span>
                      </label>
                    ))}
                  </div>
                </div>
              )}

              <div className="form-group">
                <span className="form-label">Or type custom response:</span>
                <input
                  type="text"
                  className="form-input"
                  placeholder="Custom details or decision..."
                  value={customClarifyInput}
                  onChange={(e) => setCustomClarifyInput(e.target.value)}
                  onKeyDown={(e) => e.key === 'Enter' && handleClarifySubmit()}
                />
              </div>
            </div>

            <div className="modal-footer">
              <button className="primary-btn" onClick={handleClarifySubmit}>
                Submit Answers
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="chat-input-container">
        <div className="chat-input-box">
          <textarea
            className="chat-textarea"
            placeholder={
              sessionId ? 'Ask Luna anything or assign a task (Enter to send, Shift+Enter for newline)...' : 'Open a conversation first'
            }
            disabled={!sessionId}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            rows={2}
          />
          <div className="chat-input-toolbar">
            <span style={{ fontSize: '11px', color: 'var(--text-muted)' }}>
              Markdown, tool calls, and subagent orchestration enabled
            </span>
            {turnRunning ? (
              <button className="danger-btn" onClick={onInterrupt}>
                <Square size={14} />
                <span>Stop</span>
              </button>
            ) : (
              <button
                className="primary-btn"
                onClick={handleSend}
                disabled={!sessionId || !input.trim()}
              >
                <Send size={14} />
                <span>Send</span>
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};
