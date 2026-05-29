# Model Picker: Favorites & Agents sections

Status: approved (2026-05-27)
Scope: `src/lib/ModelPicker.svelte`, `src/lib/state.svelte.ts`, new `src/lib/picker-favorites.svelte.ts`.

## Problem

The current `ModelPicker` lists models per engine in a left rail. There is no way to
pin frequently-used (agent, model) pairs or to launch a saved Agent persona from
the composer. Users have to either remember which engine hosts the model they want
or navigate to `AgentsView` to start an agent run.

## Goals

- Pin frequently-used presets so they are reachable without remembering which engine they live under.
- Launch a saved Agent persona from the composer with whatever text the user is typing.
- Keep the picker's read mental model intact: one rail, one panel.

## Non-goals

- Server-side sync of favorites across devices.
- Drag-to-reorder favorites.
- Letting users change the Agent persona of an already-spawned session.
- Showing the new sections inside `ChatView` (locked-agent picker).

## Surface

### Rail

Order, top to bottom:

1. `★` Favorites (new)
2. `⌗` Agents (new)
3. existing engine icons (`opencode`, `claude`, `gemini`, `codex`, `pi`)

Visible in `Composer` only. In `ChatView` (`lockAgent=true`) the new icons are
hidden — the rail itself is already hidden in that case.

### Rail state

`ModelPicker` replaces the current `railAgent: AgentPreset` with:

```ts
type RailView =
  | { kind: 'favorites' }
  | { kind: 'agents' }
  | { kind: 'engine'; preset: AgentPreset };
```

This unifies "which icon is active" with "what panel shows" — no more derived
mismatch when external `agent` changes.

### Right panel — Favorites mode

Two stacked subsections inside the existing scroll container:

- **Engines** — pinned engines as small cards (provider icon + label). Clicking a
  card switches the picker to that engine's default model:
  `pickAgent(preset)` then auto-pick the engine's `default` model (or first).
- **Models** — pinned `(agent, model)` pairs as rows (same row layout as the
  current model list, with engine tint chip). Clicking a row is `pickModel(a, m)`,
  identical to today.

Empty state: "No favorites yet. Star a model in any engine to pin it here."

### Right panel — Agents mode

List of saved Agents from the `management-agents` store (the existing one used
by `AgentsView`). Row layout:

- name (bold) · `slug` (mono, muted)
- preset chip + model chip (small, mono)
- one-line preview of `prompt` (truncate)

Clicking a row applies the Agent as a preset for the next spawn (see Composer
behavior below). It does NOT call `agents.run(id)` — the user's typed draft is
preserved and submitted with the persona attached.

Empty state: link/button "New agent →" that calls `onOpenBuilder?.()`. Requires
threading a new optional prop through `ModelPicker`.

Loading state: the `agents` store already exposes `loading`/`error`; render a
short "Loading…" placeholder while `loading && list.length === 0`.

### Right panel — Engine mode

Identical to today (search + scrollable model list), with one addition: each
model row gets a star-toggle button on the right edge. Hover/keyboard reveal,
not always visible — keeps the row visually quiet.

The engine header (currently provider icon + label) gets a star-toggle for the
whole engine, in the same row as the agent label.

## Storage

New file `src/lib/picker-favorites.svelte.ts`:

```ts
type Favorites = {
  engines: AgentPreset[];
  models: Array<{ agent: AgentPreset; model: string }>;
};
```

- Initial load from `localStorage` key `roy:picker:favorites`, JSON-parsed with a
  schema guard (drop unknown engines / model ids on read; keep the rest).
- Reactive `$state` object; an `$effect.root` writes back to LS on every change.
- Public API: `toggleEngine(a)`, `toggleModel(a, m)`, `hasEngine(a)`, `hasModel(a, m)`,
  plus a getter for the reactive list used by the panel.
- Order: insertion order (newest at top). No reorder UI.

## Composer behavior (Agent-as-preset)

`Composer.svelte` gains one piece of state:

```ts
let selectedAgentId = $state<string | undefined>(undefined);
```

Wired into `ModelPicker` via a new optional prop `onPickAgent(id: string)`.
Clicking an Agent row sets `selectedAgentId`, plus updates `agent` + `model` from
the Agent's `preset` + `model` so the rest of the composer state stays consistent.

`Agent.model` is nullable. When it's `null`, fall back to the engine's default
model (`engineEntry.models.find(m => m.default) ?? engineEntry.models[0]`) — the
same fallback `Composer` already uses in its initial-sync `$effect`.

Reset rules: clear `selectedAgentId` when

- the user manually picks a different model from any engine view, or
- the user manually picks a different engine card from Favorites, or
- the composer pill triggers a normal model change via the engine panel.

Pill label: when `selectedAgentId` is set, the picker trigger shows the Agent's
`name` (with a small `⌗` glyph) instead of the model label, so the user can see
which persona will spawn.

### `app.createSession` extension

`state.svelte.ts:567` `createSession(opts)` accepts two new optional fields:

```ts
opts.system_prompt?: string;
opts.agent_name?: string;
```

Both are forwarded as-is to `mgmtSessions.create(...)` — the wire already supports
them (see `CreateSessionReq` in `management-client.ts`). No daemon changes.

On submit, `Composer` looks up the selected Agent (`agents.list.find`) and passes
its `prompt` as `system_prompt` and `name` as `agent_name`. If the Agent was
deleted between selection and submit, fall back to a regular spawn and clear
`selectedAgentId`.

## ChatView

`lockAgent=true` keeps the new icons hidden. No favorite-toggle on rows either —
swapping models inside a session is still a single-click action, no need to clutter.

## Failure modes

- Stale favorites referencing a model id no longer present in `enginesConfig` →
  filtered on render, still kept in storage so re-installing the engine restores them.
- Stale favorite engines (preset no longer in catalog) → same filtering rule.
- `agents` store error → Agents panel shows the existing `agents.error` banner.
- `localStorage` unavailable (private mode quirks) → store falls back to in-memory;
  favorites work for the session, don't persist.

## Out of scope (explicit)

- Cross-device sync.
- Pin/unpin from sidebar drag-and-drop.
- Right-click context menus on rail icons.
- Showing favorites in `ChatView`.

## Open questions

None at design time. Implementation may surface CSS sizing decisions for the
star toggle (always-visible vs hover) — pick hover-visible for parity with the
existing row hover treatment.
