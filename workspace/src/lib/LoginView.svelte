<script lang="ts">
  import { authState } from './auth.svelte';
  import * as Card from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';

  let username = $state('');
  let password = $state('');
  let submitting = $state(false);

  async function onSubmit(e: SubmitEvent) {
    e.preventDefault();
    if (submitting) return;
    if (!username.trim() || !password) {
      authState.loginError = 'Username and password are required';
      return;
    }
    submitting = true;
    try {
      await authState.login(username.trim(), password);
    } catch {
      // login() already set authState.loginError; nothing to do here
    } finally {
      submitting = false;
    }
  }
</script>

<div class="flex h-dvh items-center justify-center bg-background p-4">
  <Card.Root class="w-full max-w-sm">
    <Card.Header>
      <Card.Title>Sign in</Card.Title>
      <Card.Description>roy-management session</Card.Description>
    </Card.Header>
    <Card.Content>
      <form onsubmit={onSubmit} class="space-y-3">
        <label class="block text-sm">
          <span class="mb-1 block text-muted-foreground">Username</span>
          <input
            type="text"
            autocomplete="username"
            bind:value={username}
            disabled={submitting}
            class="w-full rounded-md border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring disabled:opacity-50"
          />
        </label>
        <label class="block text-sm">
          <span class="mb-1 block text-muted-foreground">Password</span>
          <input
            type="password"
            autocomplete="current-password"
            bind:value={password}
            disabled={submitting}
            class="w-full rounded-md border bg-background px-3 py-2 text-sm focus:outline-none focus:ring-1 focus:ring-ring disabled:opacity-50"
          />
        </label>
        {#if authState.loginError}
          <p class="text-sm text-destructive" role="alert">{authState.loginError}</p>
        {/if}
        <Button type="submit" disabled={submitting} class="w-full">
          {submitting ? 'Signing in…' : 'Sign in'}
        </Button>
      </form>
    </Card.Content>
  </Card.Root>
</div>
