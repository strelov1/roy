<script lang="ts">
  import { onMount } from 'svelte';
  import { app } from './lib/state.svelte';
  import SessionList from './lib/SessionList.svelte';
  import ChatView from './lib/ChatView.svelte';
  import NewChat from './lib/NewChat.svelte';
  import ProjectView from './lib/ProjectView.svelte';
  import AgentsView from './lib/AgentsView.svelte';
  import ScheduledTasksView from './lib/ScheduledTasksView.svelte';
  import SkillsView from './lib/SkillsView.svelte';
  import ConnectionsView from './lib/ConnectionsView.svelte';
  import AcceptInviteView from './lib/AcceptInviteView.svelte';
  import { authState } from './lib/auth.svelte';
  import { royClient } from './lib/client';
  import { fetchCommandBody } from './lib/commands.svelte';
  import LoginView from './lib/LoginView.svelte';
  import * as Card from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { X, PanelLeft } from '@lucide/svelte';
  import { LS, lsGet, lsSet } from './lib/utils';

  // Sidebar visibility. Persists across reloads so the user's choice
  // sticks. Defaults to open on first visit.
  let sidebarOpen = $state<boolean>(lsGet(LS.sidebarOpen) !== 'closed');
  $effect(() => {
    lsSet(LS.sidebarOpen, sidebarOpen ? 'open' : 'closed');
  });

  const wsUrl = import.meta.env.VITE_ROY_WS_URL ?? 'ws://127.0.0.1:8787';

  // Three top-level routes:
  //   /            → NewChat
  //   /s/<id>      → ChatView opened on <id>
  //   /p/<id>      → ProjectView for the project
  // We render off `route`, not `app.currentSession`, so the first paint
  // after a hard reload of `/s/<id>` immediately renders ChatView (with
  // its own skeleton) instead of flashing NewChat while `app.connect()`
  // and `app.openSession()` complete asynchronously.
  type Route =
    | { kind: 'home' }
    | { kind: 'session'; id: string }
    | { kind: 'project'; id: string }
    | { kind: 'agents' }
    | { kind: 'scheduled' }
    | { kind: 'skills' }
    | { kind: 'connections' }
    | { kind: 'accept_invite'; token: string };

  function parseRoute(): Route {
    const session = window.location.pathname.match(/^\/s\/([^/]+)\/?$/);
    if (session) return { kind: 'session', id: session[1]! };
    const project = window.location.pathname.match(/^\/p\/([^/]+)\/?$/);
    if (project) return { kind: 'project', id: project[1]! };
    if (window.location.pathname === '/agents') return { kind: 'agents' };
    if (window.location.pathname === '/scheduled') return { kind: 'scheduled' };
    if (window.location.pathname === '/skills') return { kind: 'skills' };
    if (window.location.pathname === '/connections') return { kind: 'connections' };
    if (window.location.pathname === '/accept-invite') {
      const token = new URLSearchParams(window.location.search).get('token') ?? '';
      return { kind: 'accept_invite', token };
    }
    return { kind: 'home' };
  }

  function pathFor(r: Route): string {
    if (r.kind === 'session') return `/s/${r.id}`;
    if (r.kind === 'project') return `/p/${r.id}`;
    if (r.kind === 'agents') return '/agents';
    if (r.kind === 'scheduled') return '/scheduled';
    if (r.kind === 'skills') return '/skills';
    if (r.kind === 'connections') return '/connections';
    if (r.kind === 'accept_invite') return `/accept-invite?token=${encodeURIComponent(r.token)}`;
    return '/';
  }

  // Synchronous initial value so the very first paint reflects the URL —
  // no `currentSession`-based flicker.
  let route = $state<Route>(parseRoute());

  // Sidebar nav highlight: only these route kinds map to a sidebar item.
  const navKinds = ['agents', 'scheduled', 'skills', 'connections'] as const;
  const activeNav = $derived(
    (navKinds as readonly Route['kind'][]).includes(route.kind)
      ? (route.kind as (typeof navKinds)[number])
      : null,
  );

  let suppressUrlEffect = $state(true);
  // Latest-wins token. Popstate spam, back/forward chord, or a sidebar
  // click landing while a previous `applyRoute` is still awaiting can
  // otherwise interleave the `suppressUrlEffect` toggle.
  let routeEpoch = 0;

  async function applyRoute(r: Route) {
    const myEpoch = ++routeEpoch;
    suppressUrlEffect = true;
    route = r;
    try {
      if (r.kind === 'session') await app.openSession(r.id);
      else app.clearCurrent();
    } finally {
      // Only the last call in flight is allowed to unsuppress — earlier
      // ones leave the gate closed so the freshest transition controls
      // when the URL effect resumes.
      if (myEpoch === routeEpoch) suppressUrlEffect = false;
    }
  }

  function startNew() {
    // "+ new" in the sidebar drops us back to `/` — set the route first
    // so the layout swap is synchronous, then clear session state. If
    // we're already on home, just nudge the composer so the user can
    // start typing without a stray click.
    if (route.kind === 'home') {
      app.focusComposer();
      return;
    }
    history.pushState({}, '', '/');
    void applyRoute({ kind: 'home' });
  }

  // Single entry point for the trivial "push a path + apply route" nav.
  // `startNew`/`goHome` keep their own logic; the prop callbacks below are
  // thin wrappers so component prop names stay unchanged.
  function navigate(r: Route) {
    history.pushState({}, '', pathFor(r));
    void applyRoute(r);
  }

  const openProject = (id: string) => navigate({ kind: 'project', id });
  const openSession = (id: string) => navigate({ kind: 'session', id });
  const openAgents = () => navigate({ kind: 'agents' });
  const openScheduled = () => navigate({ kind: 'scheduled' });
  const openSkills = () => navigate({ kind: 'skills' });
  const openConnections = () => navigate({ kind: 'connections' });

  function goHome() {
    history.pushState({}, '', '/');
    return applyRoute({ kind: 'home' });
  }

  /// Slugs of the builder skills consumed by the `+` buttons. Must match
  /// directory names under `~/.roy/skills/` — if either is renamed there,
  /// update here too.
  const AGENT_BUILDER_SKILL = 'roy-agent-builder';
  const SKILL_BUILDER_SKILL = 'roy-skill-builder';

  /// Navigate home and splice the builder-skill body into the composer.
  /// Fetch first so a 404 (skill missing) keeps the user on the catalog
  /// with an error toast — no point navigating away into an empty
  /// composer when the prefill silently failed.
  async function spawnWithSkill(skillName: string) {
    const body = await fetchCommandBody(skillName);
    if (body === null) {
      app.lastError = `Skill "${skillName}" not found. Create it at ~/.roy/skills/${skillName}/SKILL.md.`;
      return;
    }
    if (route.kind !== 'home') await goHome();
    app.prefillComposer(body);
  }

  // Called by Composer/NewChat after `createSession` resolves. We flip
  // `route` to the new session id; the $effect below pushes /s/<id> into
  // the URL.
  function finishNew(id: string) {
    route = { kind: 'session', id };
  }

  $effect(() => {
    if (suppressUrlEffect) return;
    const session = app.currentSession;
    // Reflect actual session state into `route` (covers post-spawn auto-open
    // and any path where `currentSession` changes outside applyRoute).
    if (session && (route.kind !== 'session' || route.id !== session)) {
      route = { kind: 'session', id: session };
    } else if (!session && route.kind === 'session') {
      // The open session vanished (deleted or archived its journal cleared),
      // so /s/<id> now points at nothing — fall back to home.
      route = { kind: 'home' };
    }
    const target = pathFor(route);
    if (window.location.pathname !== target) {
      history.pushState({}, '', target);
    }
  });

  onMount(() => {
    // Fire-and-forget: bootstrap sets authState.{user,ws_token} which the
    // $effect below watches to trigger the WS connect. Doing the connect
    // here in addition to the effect would race — both call sites end up
    // running `royClient.connect()` in parallel, where the second close()s
    // the first's WebSocket and rejects its promise as 'closed'.
    void authState.bootstrap();
    const onPop = () => {
      void applyRoute(parseRoute());
    };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  });

  // Single source of truth for the WS connect: fires once when both
  // `user` and `ws_token` become available (after login or bootstrap),
  // and re-arms whenever the underlying socket leaves the connected /
  // connecting state — so a gateway restart auto-reconnects instead of
  // stranding the UI on the manual Retry CTA.
  let connectAttempted = $state(false);
  $effect(() => {
    const user = authState.user;
    const token = authState.ws_token;
    if (!user || !token) {
      connectAttempted = false;
      return;
    }
    // Allow a fresh attempt after a disconnect / error.
    if (app.status === 'closed' || app.status === 'error') {
      connectAttempted = false;
    }
    if (connectAttempted) return;
    connectAttempted = true;
    void (async () => {
      await app.connect(wsUrl, token);
      await applyRoute(parseRoute());
    })();
  });

  async function retryConnect() {
    const token = authState.ws_token;
    if (!token) return;
    await app.connect(wsUrl, token);
    if (app.status === 'open') {
      await applyRoute(parseRoute());
    }
  }

  async function signOut() {
    await authState.logout();
    royClient.close();
  }
</script>

{#if authState.bootstrapping}
  <div class="flex h-dvh items-center justify-center bg-background p-4">
    <span
      class="inline-block size-4 animate-spin rounded-full border-2 border-muted-foreground border-t-transparent"
      aria-hidden="true"
    ></span>
  </div>
{:else if !authState.user}
  <LoginView />
{:else if app.status === 'connecting' || app.status === 'idle'}
  <div class="flex h-dvh items-center justify-center bg-background p-4">
    <Card.Root class="w-full max-w-md">
      <Card.Header>
        <Card.Title class="flex items-center gap-2">
          <span
            class="inline-block size-3 animate-spin rounded-full border-2 border-muted-foreground border-t-transparent"
            aria-hidden="true"
          ></span>
          Connecting…
        </Card.Title>
        <Card.Description>
          Reaching the roy gateway at
          <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{wsUrl}</code>.
        </Card.Description>
      </Card.Header>
    </Card.Root>
  </div>
{:else if app.status === 'error' || app.status === 'closed' || app.connectionError}
  <div class="flex h-dvh items-center justify-center bg-background p-4">
    <Card.Root class="w-full max-w-md">
      <Card.Header>
        <Card.Title>Can't reach the roy gateway</Card.Title>
        <Card.Description>
          {app.connectionError ?? `WebSocket to ${wsUrl} is ${app.status}.`}
        </Card.Description>
      </Card.Header>
      <Card.Content class="space-y-3">
        <p class="text-sm text-muted-foreground">
          Check that <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">roy-gateway</code>
          is running with a <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">[websocket]</code>
          block in its config and that
          <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">VITE_ROY_WS_URL</code>
          in <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">roy-web/.env</code>
          points at its bind address (yours: <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{wsUrl}</code>).
        </p>
        <p class="text-xs text-muted-foreground">
          Typical start:
        </p>
        <pre class="overflow-x-auto rounded-md border bg-muted px-3 py-2 text-xs font-mono">roy serve &amp;
cargo run -p roy-gateway -- --config ~/.config/roy-gateway/telegram.toml</pre>
        <p class="text-xs text-muted-foreground">
          The gateway authenticates via the JWT issued by
          <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-[0.7rem]">/auth/login</code>;
          your session is currently bound to user
          <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-[0.7rem]">{authState.user?.username ?? '?'}</code>.
        </p>
        <div class="flex gap-2">
          <Button onclick={() => void retryConnect()}>Retry</Button>
          <Button variant="outline" onclick={() => void signOut()}>Sign out</Button>
        </div>
      </Card.Content>
    </Card.Root>
  </div>
{:else}
  <div
    class="relative h-dvh w-full overflow-hidden bg-background text-foreground"
    data-sidebar={sidebarOpen ? 'open' : 'closed'}
  >
    {#if sidebarOpen}
      <!-- Mobile-only backdrop. Hidden at md+ where the sidebar is part
           of the layout (main is pushed right via md:pl-*). -->
      <button
        type="button"
        aria-label="Close sidebar"
        onclick={() => (sidebarOpen = false)}
        class="fixed inset-0 z-30 bg-black/40 md:hidden"
      ></button>
    {/if}
    <SessionList
      onNew={startNew}
      onOpenProject={openProject}
      onOpenAgents={openAgents}
      onOpenScheduled={openScheduled}
      onOpenSkills={openSkills}
      onOpenConnections={openConnections}
      {activeNav}
      open={sidebarOpen}
      onClose={() => (sidebarOpen = false)}
      onOpen={() => (sidebarOpen = true)}
    />
    {#if !sidebarOpen && route.kind === 'home'}
      <!-- Mobile-only floating "expand" — only on NewChat, since ChatView
           carries its own inline expand button in the header. -->
      <button
        type="button"
        onclick={() => (sidebarOpen = true)}
        aria-label="Show sidebar"
        title="Show sidebar"
        class="fixed left-3 top-3 z-30 inline-flex size-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground md:hidden"
      >
        <PanelLeft class="size-4" />
      </button>
    {/if}
    <main
      class={[
        'flex h-full min-w-0 flex-col overflow-hidden transition-[padding] duration-200',
        sidebarOpen ? 'md:pl-64' : 'md:pl-14',
      ]}
    >
      {#if route.kind === 'session'}
        <ChatView onOpenSidebar={() => (sidebarOpen = true)} />
      {:else if route.kind === 'project'}
        <ProjectView
          projectId={route.id}
          onCreated={finishNew}
          onPickSession={openSession}
          onOpenSidebar={() => (sidebarOpen = true)}
          onOpenConnections={openConnections}
        />
      {:else if route.kind === 'agents'}
        <AgentsView
          onOpenSession={openSession}
          onCreateAgent={() => void spawnWithSkill(AGENT_BUILDER_SKILL)}
        />
      {:else if route.kind === 'scheduled'}
        <ScheduledTasksView onOpenSidebar={() => (sidebarOpen = true)} />
      {:else if route.kind === 'skills'}
        <SkillsView
          onCreateSkill={() => void spawnWithSkill(SKILL_BUILDER_SKILL)}
        />
      {:else if route.kind === 'connections'}
        <ConnectionsView />
      {:else if route.kind === 'accept_invite'}
        <AcceptInviteView
          token={route.token}
          onDone={goHome}
        />
      {:else}
        <NewChat onCreated={finishNew} onOpenConnections={openConnections} />
      {/if}
    </main>
  </div>
{/if}

{#if app.lastError}
  <div
    role="alert"
    class="fixed bottom-4 right-4 z-30 flex max-w-sm items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs font-medium text-destructive shadow-md backdrop-blur"
  >
    <span class="min-w-0 flex-1 break-words">{app.lastError}</span>
    <Button
      variant="ghost"
      size="icon-xs"
      onclick={() => app.dismissError()}
      aria-label="dismiss"
      class="-mr-1 -mt-0.5 text-destructive hover:bg-destructive/15 hover:text-destructive"
    >
      <X />
    </Button>
  </div>
{/if}
