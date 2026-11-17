# Product

<!-- impeccable:product-schema 1 -->

## Platform

android

## Users

- Primary user: Developers, builders, and technical power users using Luna directly on an Android smartphone (`arm64-v8a`) without Termux or a tethered PC (inferred from user prompt and repo context).
- Operating context: One-handed portrait phone usage on the go, frequently switching between active agent tasks, reviewing code/tool outputs, and managing sessions, memories, and notes.

## Product Purpose

- Luna Mobile is a standalone native Android client for the Rust-based Luna autonomous AI agent harness (`harness-core`, `harness-providers`, `harness-memory`, `harness-notes`) running in-process via JNI (`libluna_android.so`).
- Success means the user can configure an AI provider/model, converse with streaming responses and Markdown/code clarity, approve or deny mutating tool calls safely, manage conversation sessions, and inspect saved memories and notes effortlessly on a phone.

## Positioning

- Unlike cloud-only chat wrappers or Termux terminal workarounds, Luna Mobile embeds the real Rust agent runtime, SQLite session store, persistent memory engine, and Markdown notes engine natively on-device with Material Design 3 ergonomics.

## Operating Context

- Surfaces: four primary destinations — `Chat`, `Sessions`, `Memory` (Memories & Notes), and `Settings` — plus interactive approval prompts, model/provider pickers, and full note reading sheets.
- Hardware constraints: compact phone screens (`1080x2310` baseline, portrait), edge-to-edge system bars, soft keyboard (IME) overlay, light/dark system themes, and varying Android font scales.

## Capabilities and Constraints

- Confirmed capabilities (implemented in `crates/harness-android` and `android/`):
  - Multi-provider LLM streaming (OpenAI, Anthropic, Gemini, DeepSeek, Groq, NVIDIA, OpenRouter, etc.) with API key storage and live model refresh.
  - Persistent SQLite sessions (`nativeNewSession`, `nativeSelectSession`, `nativeDeleteSession`, `nativeCompactSession`) and context-token tracking.
  - Agent tool execution with interactive approval gate (Allow once, Allow for session, Deny) and turn cancellation.
  - Persistent Memory records and Markdown Notes search and listing.
- Undecided / deferred product scope: remote desktop tethering, voice/audio loop on Android, and multi-pane tablet layouts (inferred out of scope for this Android UI overhaul).

## Brand Commitments

- Name: **Luna** (`ai.luna.mobile`), paired with the crescent moon mark (`ic_luna.xml`).
- Visual DNA from Luna's desktop TUI (`crates/harness-cli/src/commands/chat/tui/theme.rs`): warm coral signature accent (`#EA7652` dark / `#D05430` light), cool technical blue (`#6AA6E8` / `#2A6EC2`), warm amber (`#E5B567` / `#B07212`), neutral slate-charcoal dark canvas (`#0E0E0F`) and crisp light paper canvas (`#F8F8FA`) — adapted cleanly into Material Design 3 color roles.

## Evidence on Hand

- Existing implementation: `android/app/src/main/java/ai/luna/mobile/MainActivity.kt`, `Models.kt`, `LunaBridge.kt`, and `crates/harness-android/src/runtime.rs`.
- Live test device connected via ADB (`192.168.43.1:5555`, Huawei JNY-LX1, `1080x2310`, `Night mode: no`, `font_scale: 0.85`).

## Product Principles

- **Operate first**: Clarity, scanability, and reliable touch controls outrank ornament; Material 3 patterns govern navigation, sheets, dialogs, and states.
- **Readable technical prose**: Long assistant replies, code blocks, reasoning traces, and tool outputs must stay crisp, scannable, and never cramped inside nested boxes.
- **Fail-safe control**: Destructive actions (deleting sessions) and mutating tool calls require clear, unambiguous confirmation without blocking routine flow.
- **Both themes, edge-to-edge**: Dark and light themes are both first-class; system bars, cutouts, and the IME keyboard never obscure content or the composer.

## Accessibility & Inclusion

- Minimum `48x48 dp` touch targets on interactive controls, `sp` typography scaling with Android `font_scale`, WCAG AA text contrast in both light and dark modes, and respect for system reduced-motion settings.
