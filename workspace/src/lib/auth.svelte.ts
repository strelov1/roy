// Auth state + HTTP helpers. The roy-management backend issues an HttpOnly
// JWT cookie on login and additionally returns the same JWT in a JS-accessible
// `ws_token` field so we can pass it through the WebSocket subprotocol slot
// (browsers can't read HttpOnly cookies from JS, and they can't set arbitrary
// headers on `new WebSocket(url, [protocols])`).

import { errMsg } from './utils';

const BASE = '/management';

export type Role = 'owner' | 'member';

export type TeamMembership = {
  id: string;
  name: string;
  role: Role;
};

export type UserProfile = {
  id: string;
  username: string;
  display_name: string;
  timezone: string | null;
  teams: TeamMembership[];
};

export type AuthResponse = {
  user: UserProfile;
  ws_token: string;
};

/** Thrown by login/me/logout when the HTTP call fails. `status` is the response
 *  code (401 for bad credentials / expired session, 429 for rate-limit, etc.). */
export class AuthError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(`${status}: ${message}`);
    this.name = 'AuthError';
  }
}

async function postJson<T>(path: string, body: unknown, expectStatus?: number): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'POST',
    credentials: 'include',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (expectStatus !== undefined ? res.status !== expectStatus : !res.ok) {
    let msg = res.statusText;
    try {
      const b = await res.json();
      if (b && typeof b.error === 'string') msg = b.error;
    } catch {
      // body wasn't JSON; keep status text
    }
    throw new AuthError(res.status, msg);
  }
  if (res.status === 204) return undefined as unknown as T;
  return (await res.json()) as T;
}

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    method: 'GET',
    credentials: 'include',
    headers: { 'content-type': 'application/json' },
  });
  if (!res.ok) {
    let msg = res.statusText;
    try {
      const b = await res.json();
      if (b && typeof b.error === 'string') msg = b.error;
    } catch {
      // not JSON
    }
    throw new AuthError(res.status, msg);
  }
  return (await res.json()) as T;
}

export const auth = {
  /** POST /auth/login — sets the HttpOnly cookie and returns the profile +
   *  the ws_token for the WS subprotocol. */
  login: (username: string, password: string) =>
    postJson<AuthResponse>('/auth/login', { username, password }),

  /** POST /auth/logout — clears the cookie server-side (204). */
  logout: () => postJson<void>('/auth/logout', {}, 204),

  /** GET /auth/me — works whenever the cookie is set; returns the same
   *  shape as login. Used on page load to detect an already-authenticated
   *  session and to recover the ws_token after a hard refresh. */
  me: () => getJson<AuthResponse>('/auth/me'),
};

/** Single global auth state. Pages render off this. `user === null` means
 *  "not logged in" (show the login screen); `user !== null` means we have
 *  a valid session and can connect the WebSocket using `ws_token`. */
class AuthStateImpl {
  user = $state<UserProfile | null>(null);
  ws_token = $state<string | null>(null);
  /** `true` while the initial /auth/me probe is in flight on page load.
   *  Renders a spinner instead of flashing the login screen for a frame. */
  bootstrapping = $state(true);
  /** Last error from a login attempt — surfaced inline in the form. */
  loginError = $state<string | null>(null);

  /** Try to recover an existing session from the cookie. Called once on mount.
   *  Sets `bootstrapping = false` when done regardless of outcome. */
  async bootstrap(): Promise<void> {
    try {
      const me = await auth.me();
      this.user = me.user;
      this.ws_token = me.ws_token;
    } catch (e) {
      if (!(e instanceof AuthError && e.status === 401)) {
        // 401 = no cookie / expired, expected path. Anything else is worth
        // surfacing.
        console.warn('roy-web: /auth/me failed', e);
      }
      this.user = null;
      this.ws_token = null;
    } finally {
      this.bootstrapping = false;
    }
  }

  async login(username: string, password: string): Promise<void> {
    this.loginError = null;
    try {
      const res = await auth.login(username, password);
      this.user = res.user;
      this.ws_token = res.ws_token;
    } catch (e) {
      if (e instanceof AuthError) {
        this.loginError = e.status === 401 ? 'Invalid username or password' : `Login failed: ${e.message}`;
      } else {
        this.loginError = errMsg(e);
      }
      throw e;
    }
  }

  /** Locally patch `user.teams` without hitting `/auth/me`. Used after
   *  team CRUD when the caller already knows the new membership shape. */
  patchTeams(next: TeamMembership[]): void {
    if (!this.user) return;
    this.user = { ...this.user, teams: next };
  }

  /** Re-fetch `/auth/me`. Use when the membership delta isn't obvious
   *  (e.g. invite acceptance, where the joined team's role we'd need to
   *  invent client-side anyway). On failure we leave the cached profile —
   *  a real 401 still surfaces through the next protected request. */
  async refresh(): Promise<void> {
    try {
      const me = await auth.me();
      this.user = me.user;
      this.ws_token = me.ws_token;
    } catch (e) {
      console.warn('roy-web: /auth/me refresh failed', e);
    }
  }

  async logout(): Promise<void> {
    try {
      await auth.logout();
    } catch (e) {
      console.warn('roy-web: /auth/logout failed', e);
    }
    this.user = null;
    this.ws_token = null;
  }
}

export const authState = new AuthStateImpl();
