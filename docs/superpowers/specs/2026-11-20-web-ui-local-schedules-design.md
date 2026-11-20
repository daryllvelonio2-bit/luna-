# Web UI refinement and device-local automation design

Date: 2026-11-20
Status: Proposed for approval

## Objective

Refine Luna's existing web workspace so common tasks are clearer on desktop and mobile, and make newly requested automations run at the device-local wall-clock time the user states. Saying "8pm" must create a schedule for 8:00 PM local time without asking the user to convert to UTC.

## Selected direction

Use a focused refinement of the current product UI, not a new visual identity. Keep the existing navigation, sessions, chat, automation, plugin, memory, settings, light/dark themes, and API contracts. Reduce density and ambiguity where it interferes with those tasks.

Two implementation approaches were considered:

1. Refine the incumbent interface and add explicit device-local schedule rules. This preserves existing behavior and features while fixing the main workflow problems.
2. Replace the visual system and change all existing UTC rules to local time. This has a larger regression surface and silently changes persisted schedules.

The user selected the focused refinement. Existing UTC schedules must not change meaning.

## Product principles

- Task completion takes priority over decoration.
- Use the existing system-font product vocabulary and restrained color palette.
- Preserve familiar chat, navigation, form, and scheduling affordances.
- Make timing visible in plain language before saving.
- Do not require timezone knowledge for ordinary local schedules.

## Web UI/UX changes

### Shared workspace

- Improve page and section hierarchy with consistent spacing, readable titles, and quieter secondary metadata.
- Make navigation expose the current page through `aria-current` and clear selected styling.
- Add a mobile sidebar backdrop, close the drawer after navigation or workspace selection, support Escape dismissal, and keep the main content usable at narrow widths.
- Increase small interactive targets where practical and make keyboard focus consistently visible.
- Keep both light and dark themes and respect reduced motion.

### Chat and composer

- Reduce competing metadata and clarify the primary message action without removing model, reasoning, mode, workspace, context, attachment, queue, steer, or stop controls.
- Give the conversation a clearer readable column and keep the composer visually anchored.
- Use explicit accessible names for icon-only actions and dialogs.

### Automations

- Replace "New automation routes into chat" with a direct New automation editor using the same proven editor component as Edit.
- Present schedule choices as Daily, Interval, One time, and Cron, with ordinary controls first and cron details progressively disclosed.
- Default daily and cron schedules to device-local time. Show the detected device timezone and a plain-language next-run preview before saving.
- Preserve legacy UTC rules when editing existing tasks, and label them as UTC.
- Keep search, active/paused/history views, run now, pause/resume, edit, delete confirmation, and run history.
- Make cards and forms responsive below 400px content widths; remove ID-chip noise from primary scanning while retaining identifiers in details/history.
- Provide useful loading, empty, error, saving, running, and delete states.

## Scheduling behavior

### Rule vocabulary

Add two local wall-clock rule forms while retaining current forms for compatibility:

- `daily-local:HH:MM` — every day at that time in the device's current local timezone.
- `local-cron:<min> <hour> <dom> <mon> <dow>` — cron fields evaluated against device-local time.

Retain these existing forms unchanged:

- `daily:HH:MM` — UTC daily.
- bare five-field cron — UTC cron.
- `every:<seconds>` — fixed elapsed interval.
- `once:<RFC3339>` — one-shot timestamp with its explicit offset.

The web editor and the model-facing scheduling instructions will use local rules for new wall-clock schedules. No timezone clarification is needed for an ordinary phrase such as "every day at 8pm". Explicit UTC requests continue to use the existing UTC forms.

### Device definition

The scheduler executes on the device running the Luna process, so "device local" means that process's operating-system timezone via `chrono::Local`. The web interface will state this as "Luna device time" rather than implying that a remote browser overrides the server's timezone.

### DST and timezone changes

- Compute each next run from the current local timezone, so later recalculations follow OS timezone changes.
- For a nonexistent daily time during a spring-forward transition, advance to the first valid local minute rather than failing or silently switching to UTC.
- For an ambiguous daily time during fall-back, use the earliest occurrence.
- Local cron scans actual local minutes, including offset transitions, and preserves the existing 366-day search bound.
- Stored `next_run_at` values remain UTC instants for persistence and due-task comparison; only wall-clock rule interpretation changes.

## Testing and verification

Follow test-driven development for scheduler behavior and extracted web schedule helpers.

Scheduler tests:

- Prove `daily-local:20:00` resolves to 8pm in a non-UTC test timezone.
- Prove legacy `daily:20:00` remains UTC.
- Prove local cron uses local fields and bare cron remains UTC.
- Cover invalid local rule forms.
- Cover spring-forward nonexistent times and fall-back ambiguous times.

Web tests:

- Prove daily/cron parsing and serialization preserve local versus UTC semantics.
- Prove the editor creates `daily-local:20:00` for an 8pm selection.
- Prove human-readable schedule summaries identify local time and legacy UTC correctly.
- Run TypeScript typecheck and production build.

Repository verification:

- Run focused tests during implementation where practical.
- Run the required full build and test suite through GitHub Actions CI on a branch based on `main`.
- Run the Impeccable mechanical detector on changed web files.
- Inspect the rendered desktop and mobile automation workflows in a browser.
- Report the CI run and any remaining limitations honestly.

## Explicit non-goals

- No new visual brand, font package, UI framework, routing framework, or state-management dependency.
- No redesign of provider internals, memory, plugins, voice, or unrelated CLI behavior.
- No automatic migration of existing UTC schedules.
- No browser-selected timezone database or remote multi-device timezone feature.
