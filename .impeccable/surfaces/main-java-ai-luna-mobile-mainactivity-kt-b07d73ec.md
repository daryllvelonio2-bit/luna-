---
version: 1
slug: "main-java-ai-luna-mobile-mainactivity-kt-b07d73ec"
primary_target: "android/app/src/main/java/ai/luna/mobile/MainActivity.kt"
related_targets: []
---

# Surface Brief: Luna Mobile (Android)

- **Target:** `android/app/src/main/java/ai/luna/mobile/MainActivity.kt`
- **Platform / Mode:** `android` · `operate` (code-led, unattended autonomous execution; assumptions inferred from `PRODUCT.md` and existing code)
- **Primary Task:** Chat with Luna using streaming Markdown responses, inspect/approve tool executions, switch/manage persistent sessions, browse memories and notes, and configure providers/models/appearance in Settings.

## Direction contract

- **THESIS:** Replace generic neon-purple bubble clutter with an obsidian/daylight editorial observatory logbook where assistant prose flows directly on the canvas with crisp Markdown/code plates, while Material 3 tonal layers, sheets, and a live Context Telemetry Pill keep agent state instantly legible one-handed.
- **OWN-WORLD:** Deep obsidian dark (`#0C0D10`, elevated `#16181E`, code `#12141A`) and daylight paper light (`#F7F7FA`, elevated `#FFFFFF`, code `#ECEDF2`), Luna coral (`#EA7652` dark / `#D05430` light) strictly for primary actions, active indicators, and context pulse; celestial blue (`#6AA6E8` / `#2A6EC2`) for reasoning/tools; Material 3 Typography roles (`sp`-scaled, tabular numbers for tokens/counts, monospace strictly for code/IDs/paths). Raised by exposure-record zone discipline (clear tonal step hierarchy) and orienteering legend discipline (strict semantic state colors).
- **STORY:** On first launch without a key, the user lands on a clear onboarding prompt or Chat empty state with a direct "Configure Provider" action; once active, they immediately see the active model, context usage %, clean Markdown replies, grouped expandable tool/thinking steps, and a thumb-friendly floating pill composer with IME-aware padding.
- **FIRST VIEWPORT:** Compact Material 3 `TopAppBar` with Luna crescent mark, screen title, active `provider/model` subtitle, tappable Context Telemetry Pill (progress ring + `%` -> opens Context & Compact sheet), and New Chat action; full-height scrollable transcript (or welcoming start canvas with starter prompt chips); docked pill composer at bottom (`imePadding()`); Material 3 `NavigationBar` (`Chat`, `Sessions`, `Memory`, `Settings`) with 48dp+ targets and edge-to-edge system bar insets.
- **FORM:** Precision Observatory Logbook (candidate 7 of 7; `seed:e197e2c0d8d1105a`).
- **FINISH:** unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
