import { authState, type TeamMembership } from './auth.svelte';

// HTTP client for roy-management's CRUD API (axum, default :8079).
// In dev, requests go through the Vite proxy under `/management/*`. In a
// hypothetical prod deploy the same paths can be served same-origin or via
// a reverse proxy — the client only knows about the `/management` prefix.
//
// Naming note: roy-web has two unrelated things called "agent" — the daemon's
// harness catalog (`HarnessInfo` in wire.ts, used by `harnessesConfig`) and
// the user-defined agent identities served by roy-management. The types
// here describe the latter and are kept in this dedicated module to avoid
// mixing with the harness-catalog code.

const BASE = '/management';

/** Mirrors `meta_store::Project` (Rust). `path` is the absolute workspace
 *  directory; roy-management `mkdir -p`s it at creation time. `team_id` is
 *  `null` for personal projects, otherwise the team that owns the project. */
export type Project = {
  id: string;
  name: string;
  path: string;
  team_id: string | null;
  created_at: number;
};

/** Body for `POST /sessions`. Either `project_id` or `cwd` resolves the
 *  spawn dir; project wins if both are set. `scope` + `team_id` pick the
 *  workspace root (defaults to personal). */
export type CreateSessionReq = {
  harness: string;
  scope?: 'personal' | 'team';
  team_id?: string;
  project_id?: string;
  cwd?: string;
  model?: string;
  permission?: string;
  system_prompt?: string;
  agent_name?: string;
  tags?: Record<string, string>;
  /** MCP connections to attach to the spawned session. Daemon-side rejects
   *  non-claude presets with a non-empty list (502 → user-friendly error
   *  surfaced through `HttpError.message`). */
  connection_ids?: string[];
};

/** Response of `POST /sessions`. */
export type CreatedSession = {
  session_id: string;
  project_id: string | null;
  tags: Record<string, string>;
  agent_name: string | null;
};

/** Element of `GET /sessions` — rich join across daemon's live/archived
 *  list and management's session_meta. `live: false` means archived. */
export type SessionMetaRow = {
  session_id: string;
  project_id: string | null;
  agent_name: string | null;
  tags: Record<string, string>;
  live: boolean;
};

export class HttpError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(`${status}: ${message}`);
    this.name = 'HttpError';
  }
}

async function request<T>(
  path: string,
  init: RequestInit & { expectStatus?: number } = {},
): Promise<T> {
  const { expectStatus, ...rest } = init;
  // FormData must set its own multipart boundary in `content-type`; forcing
  // application/json would corrupt the boundary string.
  const isFormData = rest.body instanceof FormData;
  const res = await fetch(`${BASE}${path}`, {
    ...rest,
    credentials: 'include',
    headers: isFormData
      ? rest.headers
      : { 'content-type': 'application/json', ...(rest.headers ?? {}) },
  });
  if (res.status === 401) {
    // Cookie expired or missing. Wipe local auth state so the App.svelte
    // gate falls back to the login screen on the next reactive read.
    // Guarded against the multi-fetch-on-mount cascade: every parallel
    // 401 would otherwise re-write the same nulls and re-fire every
    // $effect that reads authState.
    if (authState.user !== null || authState.ws_token !== null) {
      authState.user = null;
      authState.ws_token = null;
    }
    throw new HttpError(401, 'auth required');
  }
  if (expectStatus !== undefined ? res.status !== expectStatus : !res.ok) {
    let msg = res.statusText;
    try {
      const body = await res.json();
      if (body && typeof body.error === 'string') msg = body.error;
    } catch {
      // body wasn't JSON; keep status text
    }
    throw new HttpError(res.status, msg);
  }
  // 204 No Content for delete — return undefined-as-any so callers can ignore it.
  if (res.status === 204) return undefined as unknown as T;
  return (await res.json()) as T;
}

/** Mirrors `roy_management::connections::Connection`. `config` is
 *  kind-specific (mcp_stdio → command/args/env; telegram_bot → {}). No UI
 *  reads `config` off a fetched connection, so it's left as an open record. */
export type Connection = {
  id: string;
  owner_id: string;
  name: string;
  slug: string;
  kind: 'mcp_stdio' | 'telegram_bot';
  config: Record<string, unknown>;
  secrets: Record<string, string> | null;
  description: string | null;
  created_at: number;
  updated_at: number;
  provider_id: string | null;
};

export type McpStdioConfig = {
  command: string;
  args?: string[];
  env?: Record<string, string>;
};

export type ProviderSecretSchema = {
  key: string;
  label: string;
  help?: string | null;
};

/** Mirrors `roy_management::provider_catalog::Provider`. Read-only catalog
 *  entry — backend resolves command/args/env from this when the client POSTs
 *  a catalog-backed connection. */
export type Provider = {
  id: string;
  name: string;
  description: string;
  icon: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  secrets: ProviderSecretSchema[];
};

/** Catalog-backed POST body. Backend resolves command/args/env from yaml. */
export type NewConnectionFromProvider = {
  provider_id: string;
  name: string;
  secrets: Record<string, string>;
};

/** Free-form POST body — used by the custom-MCP dialog and the Telegram
 *  bot create flow. */
export type NewConnectionCustom = {
  name: string;
  kind: 'mcp_stdio' | 'telegram_bot';
  config: Record<string, unknown>;
  secrets?: Record<string, string> | null;
  description?: string | null;
};

export type NewConnection = NewConnectionFromProvider | NewConnectionCustom;

/** PUT body. Per Rust's double-Option pattern: omit a key to leave alone,
 *  pass `null` to clear, pass a value to set. `name` and `config` are plain
 *  optional (omit-or-set; can't be cleared to null). */
export type ConnectionUpdate = {
  name?: string;
  config?: McpStdioConfig;
  secrets?: Record<string, string> | null;
  description?: string | null;
};

export const connections = {
  list: () => request<Connection[]>('/connections'),
  create: (body: NewConnection) =>
    request<Connection>('/connections', {
      method: 'POST',
      body: JSON.stringify(body),
      expectStatus: 201,
    }),
  get: (id: string) =>
    request<Connection>(`/connections/${encodeURIComponent(id)}`),
  update: (id: string, body: ConnectionUpdate) =>
    request<Connection>(`/connections/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  remove: (id: string) =>
    request<void>(`/connections/${encodeURIComponent(id)}`, {
      method: 'DELETE',
      expectStatus: 204,
    }),
};

export type SessionStrategy = 'ephemeral' | 'persistent_one' | 'per_sender_sticky';

/** Mirrors `roy_management::channel_bindings::ChannelBinding`. */
export type ChannelBinding = {
  id: string;
  owner_id: string;
  channel_kind: string; // "telegram"
  connection_id: string;
  agent_slug: string;
  agent_scope: string; // "user" | "team:<team_id>"
  session_strategy: SessionStrategy;
  idle_timeout_secs: number | null;
  allowed_user_ids: number[];
  enabled: boolean;
  created_at: number;
  updated_at: number;
};

/** Body for `POST /channel-bindings`. */
export type NewChannelBinding = {
  connection_id: string;
  agent_slug: string;
  agent_scope: string;
  session_strategy: SessionStrategy;
  idle_timeout_secs?: number;
  allowed_user_ids?: number[];
};

export const channelBindings = {
  list: () => request<ChannelBinding[]>('/channel-bindings'),
  create: (body: NewChannelBinding) =>
    request<ChannelBinding>('/channel-bindings', {
      method: 'POST',
      body: JSON.stringify(body),
      expectStatus: 201,
    }),
  remove: (id: string) =>
    request<void>(`/channel-bindings/${encodeURIComponent(id)}`, {
      method: 'DELETE',
      expectStatus: 204,
    }),
  setEnabled: (id: string, enabled: boolean) =>
    request<ChannelBinding>(`/channel-bindings/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      body: JSON.stringify({ enabled }),
    }),
};

export const providers = {
  list: () => request<Provider[]>('/providers'),
};

/** Slash-command catalog entry. Mirrors the JSON served by
 *  `GET /management/commands` (a scan of skills + plugin marketplaces). */
export type CommandInfo = {
  name: string;
  description: string;
  source: string;
};

/** Body returned by `GET /management/commands/:name`. */
export type CommandBody = {
  name: string;
  body: string;
};

/** `POST /management/commands` body — creates a skill under ~/.roy/skills/. */
export type CreateCommandReq = {
  name: string;
  description: string;
  body: string;
};

export const commands = {
  list: () => request<CommandInfo[]>('/commands'),
  body: (name: string) =>
    request<CommandBody>(`/commands/${encodeURIComponent(name)}`),
  create: (req: CreateCommandReq) =>
    request<void>('/commands', { method: 'POST', body: JSON.stringify(req) }),
};

/** Raw `/management/agents` row. `slug` is the `.md` file stem (stable id
 *  used by channel bindings); `name` is the frontmatter display name. */
export type WireAgent = {
  slug: string;
  name: string;
  description: string;
  harness: string;
  model?: string | null;
  body: string;
  scope: { kind: string; team_id?: string };
};

export const agents = {
  list: () => request<WireAgent[]>('/agents'),
};

export const projects = {
  list: () => request<Project[]>('/projects'),
  create: (name: string, team_id?: string) =>
    request<Project>('/projects', {
      method: 'POST',
      body: JSON.stringify(team_id ? { name, team_id } : { name }),
      expectStatus: 201,
    }),
  /** Update project metadata. Tri-state on `team_id` mirrors the AgentPatch
   *  pattern (see top of file): omit to leave alone, `null` to clear to
   *  personal, string value to move to that team. `name` is regular optional
   *  (omit or set; never null). */
  update: (
    id: string,
    body: { name?: string; team_id?: string | null },
  ) =>
    request<Project>(`/projects/${encodeURIComponent(id)}`, {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  remove: (id: string) =>
    request<void>(`/projects/${encodeURIComponent(id)}`, {
      method: 'DELETE',
      expectStatus: 204,
    }),
};

/** Mirrors `roy_scheduler::types::Agent`. Snake_case to match Rust serde.
 *  Persistent is SQLite INTEGER 0/1, serialized as a number — NOT a JS boolean. */
export type SchedulerAgent = {
  id: string;
  name: string;
  harness: string;
  project_id: string | null;
  task: string;
  model: string | null;
  persistent: number;
  persistent_session_id: string | null;
  notify_session: string | null;
  created_at: string;
  updated_at: string;
};

export type TriggerKind = 'cron' | 'oneshot';
export type FireStatus = 'running' | 'ok' | 'error' | 'timeout';

/** Mirrors `roy_scheduler::types::Trigger`. */
export type SchedulerTrigger = {
  id: string;
  agent_id: string;
  kind: TriggerKind;
  cron_expr: string | null;
  timezone: string;
  fire_at: string | null;
  next_fire_at: string;
  last_fire_at: string | null;
  paused: number;
  last_error: string | null;
  created_at: string;
};

/** Mirrors `roy_scheduler::types::Fire`. */
export type SchedulerFire = {
  id: string;
  agent_id: string;
  trigger_id: string | null;
  session_id: string | null;
  status: FireStatus;
  started_at: string;
  finished_at: string | null;
  transcript_seq_range_start: number | null;
  transcript_seq_range_end: number | null;
  assistant_text: string | null;
  cost_usd: number | null;
  stop_reason: string | null;
  error_message: string | null;
};

type SchedListOpts = { agentId?: string; limit?: number };

function schedQs(opts?: SchedListOpts): string {
  const qs = new URLSearchParams();
  if (opts?.agentId) qs.set('agent', opts.agentId);
  if (opts?.limit) qs.set('limit', String(opts.limit));
  return qs.toString();
}

/** Read-only client for the scheduler endpoints in roy-management. Throws
 *  `HttpError` with status 503 when the scheduler DB isn't attached. */
export const scheduler = {
  agents: () => request<SchedulerAgent[]>('/scheduler/agents'),
  triggers: (opts?: SchedListOpts) => {
    const qs = schedQs(opts);
    return request<SchedulerTrigger[]>(
      qs ? `/scheduler/triggers?${qs}` : '/scheduler/triggers',
    );
  },
  fires: (opts?: SchedListOpts) => {
    const qs = schedQs(opts);
    return request<SchedulerFire[]>(qs ? `/scheduler/fires?${qs}` : '/scheduler/fires');
  },
};

export const sessions = {
  /** Fetch the joined live+archived index with project_id / agent_name /
   *  tags. roy-web merges this onto daemon's `list`/`list_archived` results
   *  so existing components keep reading `SessionInfo.project_id` / `tags`. */
  list: () => request<SessionMetaRow[]>('/sessions'),
  create: (req: CreateSessionReq) =>
    request<CreatedSession>('/sessions', {
      method: 'POST',
      body: JSON.stringify(req),
      expectStatus: 201,
    }),
  putTags: (id: string, tags: Record<string, string>) =>
    request<void>(`/sessions/${encodeURIComponent(id)}/tags`, {
      method: 'PUT',
      body: JSON.stringify({ tags }),
    }),
};

/** Server-side `roy_auth::Team` row (full record from `POST /teams`). The
 *  list endpoint returns memberships, which are typed as `TeamMembership`
 *  in `auth.svelte.ts` — that's the single shape every consumer reads. */
export type Team = {
  id: string;
  name: string;
  description: string | null;
  created_by: string | null;
  created_at: number;
};

export type CreatedInvite = { token: string; team_id: string };

export const teams = {
  list: () => request<TeamMembership[]>('/teams'),
  create: (name: string, description?: string) =>
    request<Team>('/teams', {
      method: 'POST',
      body: JSON.stringify(description ? { name, description } : { name }),
    }),
  remove: (id: string) =>
    request<void>(`/teams/${encodeURIComponent(id)}`, {
      method: 'DELETE',
      expectStatus: 204,
    }),
  /** Owner-only. `expires_at` is Unix ms; omit for a non-expiring invite. */
  createInvite: (team_id: string, expires_at?: number) =>
    request<CreatedInvite>('/auth/invites', {
      method: 'POST',
      body: JSON.stringify(expires_at ? { team_id, expires_at } : { team_id }),
    }),
  /** Consume an invite token. Resolves with the joined team's id. */
  acceptInvite: (token: string) =>
    request<{ team_id: string }>('/auth/accept-invite', {
      method: 'POST',
      body: JSON.stringify({ token }),
    }),
};

/** Upper bound on a single upload — must match the `DefaultBodyLimit` set on
 *  `POST /uploads` in `roy-management/src/http.rs`. The MB form is used in
 *  user-facing strings so the two limits stay in lockstep. */
export const MAX_UPLOAD_MB = 25;

export type UploadResp = { path: string; name: string; size: number };

export const uploads = {
  send: (file: File): Promise<UploadResp> => {
    const fd = new FormData();
    fd.append('file', file);
    return request<UploadResp>('/uploads', { method: 'POST', body: fd });
  },
};
