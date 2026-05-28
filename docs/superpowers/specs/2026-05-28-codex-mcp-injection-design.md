# Codex MCP Injection — Design

**Date:** 2026-05-28
**Status:** Approved (pending user sign-off)
**Builds on:** `docs/superpowers/specs/2026-05-27-connection-catalog-design.md` + opencode/gemini extensions

## Problem

`codex-acp` (Zed Industries' ACP adapter for OpenAI Codex) is the third major preset alongside claude and opencode. After this design lands, `claude + opencode + gemini + codex` will all support MCP connection injection from the catalog UI. Only `pi` remains permanently unsupported (its README explicitly says "No MCP. Build CLI tools with READMEs instead.").

Codex doesn't read MCP config from `<cwd>/*.json` like claude/opencode/gemini. Its config lives at `~/.codex/config.toml` under `[mcp_servers]`. Editing that file per-session is unsafe (multi-session race, contaminates global state, requires HOME isolation).

## Decision: `-c` argv overrides, no file writes

`codex-acp` accepts `-c <key>=<value>` CLI flags that override individual TOML keys. Verified empirically:

```bash
codex-acp -c 'mcp_servers.roy-connections.command="roy"' \
          -c 'mcp_servers.roy-connections.args=["mcp","serve-connections","--specs","/tmp/bundle.json"]'
```

`codex-acp --help` documents the syntax:

> The `value` portion is parsed as TOML. If it fails to parse as TOML, the raw string is used as a literal.
> Examples:
> - `-c model="o3"`
> - `-c 'sandbox_permissions=["disk-full-read-access"]'`
> - `-c shell_environment_policy.inherit=all`

This is **purely per-session**, **no file system writes** (in cwd or HOME), **no race conditions**, **no auth corruption** (because we don't touch `~/.codex/auth.json`). Cleanest option of the four presets — even cleaner than claude/opencode/gemini, which all write files.

## What changes

### New `McpInjectionStyle` variant

```rust
/// codex-acp: passes `-c mcp_servers.roy-connections.command=...` and
/// `-c mcp_servers.roy-connections.args=[...]` on the spawn command line.
/// No file system writes — codex parses these as TOML overrides into its
/// in-memory Config without touching ~/.codex/config.toml.
CodexCliOverrides,
```

### `AcpConfig` refactor: dispatch evolves

The current dispatch in `AcpTransport::open` is:

```rust
let (cfg_path, cfg_value) = match self.config.mcp_injection {
    ClaudeMcpJson => (cwd.join(MCP_CONFIG_FILENAME), build_mcp_config(...)),
    OpencodeJson => (cwd.join(OPENCODE_CONFIG_FILENAME), build_opencode_config(...)),
    GeminiSettings => {
        let dir = cwd.join(GEMINI_SETTINGS_DIR);
        std::fs::create_dir_all(&dir).map_err(RoyError::Io)?;
        (dir.join(GEMINI_SETTINGS_FILENAME), build_gemini_config(...))
    }
    None => unreachable!(),
};
let cfg_text = serde_json::to_vec_pretty(&cfg_value).map_err(...)?;
std::fs::write(&cfg_path, cfg_text).map_err(RoyError::Io)?;
```

After codex lands, this must support two flavors of injection: **file-based** (claude/opencode/gemini) and **argv-based** (codex). The cleanest refactor:

```rust
enum InjectionAction {
    /// Write `value` (JSON-serialized) to `path`.
    WriteFile { path: PathBuf, value: serde_json::Value },
    /// Append these args to the child command before spawning.
    ExtraArgs(Vec<String>),
}

let action = match self.config.mcp_injection {
    ClaudeMcpJson => InjectionAction::WriteFile {
        path: cwd.join(MCP_CONFIG_FILENAME),
        value: build_mcp_config(&roy_binary_path(), &bundle_path),
    },
    OpencodeJson => InjectionAction::WriteFile {
        path: cwd.join(OPENCODE_CONFIG_FILENAME),
        value: build_opencode_config(&roy_binary_path(), &bundle_path),
    },
    GeminiSettings => {
        let dir = cwd.join(GEMINI_SETTINGS_DIR);
        std::fs::create_dir_all(&dir).map_err(RoyError::Io)?;
        InjectionAction::WriteFile {
            path: dir.join(GEMINI_SETTINGS_FILENAME),
            value: build_gemini_config(&roy_binary_path(), &bundle_path),
        }
    }
    CodexCliOverrides => InjectionAction::ExtraArgs(build_codex_args(
        &roy_binary_path(),
        &bundle_path,
    )),
    None => unreachable!(),
};

match action {
    InjectionAction::WriteFile { path, value } => {
        std::fs::write(&path, serde_json::to_vec_pretty(&value).map_err(...)?)
            .map_err(RoyError::Io)?;
    }
    InjectionAction::ExtraArgs(extra) => {
        // Appended to AcpConfig.args before spawn. See below.
        cmd_extra_args = Some(extra);
    }
}
```

And in the existing `Command::new(...).args(...)` block, append `cmd_extra_args` if present.

### `build_codex_args` helper

In `mcp_injection.rs`:

```rust
/// Build the `-c` flags codex-acp consumes. Each pair is `-c key=toml-value`.
/// `value` is parsed by codex's CliConfigOverrides as TOML; if parse fails,
/// codex falls back to the raw string. We use TOML syntax explicitly so the
/// command/args are interpreted as the right types.
pub fn build_codex_args(roy_binary: &str, bundle_path: &Path) -> Vec<String> {
    let bundle = bundle_path.to_string_lossy();
    let args_toml = format!(
        r#"["mcp","serve-connections","--specs","{}"]"#,
        bundle.replace('"', r#"\""#)
    );
    let cmd_toml = format!(r#""{}""#, roy_binary.replace('"', r#"\""#));
    vec![
        "-c".into(),
        format!("mcp_servers.roy-connections.command={cmd_toml}"),
        "-c".into(),
        format!("mcp_servers.roy-connections.args={args_toml}"),
    ]
}
```

The escape handling is conservative — quoting is TOML-style; backslash-escape any embedded `"` in paths. Bundle paths are `/var/folders/.../roy-mcp-bundle-<uuid>.json` so they don't contain `"` in practice, but the escape is defensive.

### Test

```rust
#[test]
fn codex_args_shape() {
    let v = build_codex_args("/usr/local/bin/roy", &PathBuf::from("/tmp/b.json"));
    assert_eq!(v[0], "-c");
    assert_eq!(v[1], r#"mcp_servers.roy-connections.command="/usr/local/bin/roy""#);
    assert_eq!(v[2], "-c");
    assert_eq!(
        v[3],
        r#"mcp_servers.roy-connections.args=["mcp","serve-connections","--specs","/tmp/b.json"]"#
    );
}
```

### `AcpConfig::codex()` opts in

Change `mcp_injection: McpInjectionStyle::None` to `McpInjectionStyle::CodexCliOverrides`.

Update the preset-style assertion test:

```rust
assert_eq!(AcpConfig::codex().mcp_injection, CodexCliOverrides);
```

(Pi stays None — permanently.)

### Frontend

`Composer.svelte` `MCP_PRESETS` set extends to:

```ts
const MCP_PRESETS = new Set<AgentPreset>(['claude', 'opencode', 'gemini', 'codex']);
```

`Composer.svelte` is in `/Users/i_strelov/Projects/roy-web-catalog` (the catalog UI worktree).

### CLAUDE.md

```markdown
**Connections MVP status (2026-05-29):** stdio upstream only; claude + opencode + gemini + codex presets; pi remains unsupported by design (per its README); plain-text secrets in DB; tools snapshot at spawn; resume does not re-attach connections; no `always_attach` flag yet.
```

## What's deliberately NOT included

- **OAuth for codex MCP servers** — not in MVP. Same scope cap as claude/opencode/gemini: only API-key auth via env vars / config.
- **codex tool autoapproval policies** — codex's permission system is orthogonal. Our proxy speaks plain MCP, codex applies its own approve rules per its config.
- **codex `auto-edit` / `model` overrides** — out of scope. We only inject `mcp_servers.*`.
- **HOME isolation** — explicitly avoided. The whole point of using `-c` is that we don't need it.

## Manual verification plan

1. Run `roy auth login` then `POST /sessions` with `harness: "codex"` and `connection_ids: [...]`.
2. Confirm child process tree: `ps aux | grep codex-acp` shows the spawn argv includes the two `-c` flags.
3. Confirm child process tree: `ps aux | grep "roy mcp serve-connections"` shows the proxy is alive (spawned by codex when it parsed the overrides and connected).
4. Drive a turn: ask codex to use a tool from the connection. Tools should appear with `<slug>__<tool>` namespacing.

## Test coverage in this change

- Unit: `codex_args_shape` (TOML formatting + escape).
- Unit: `presets_advertise_mcp_injection_style_correctly` extends with codex assertion.
- Integration: no new integration test for codex specifically — the existing `connections_http` covers POST flow regardless of harness; the new variant is verified by the existing acp transport tests with the appropriate fake.

## Architecture diagram (post-change)

```
preset            injection channel                       file written?
─────────────────────────────────────────────────────────────────────────
claude            <cwd>/.mcp.json                         yes
opencode          <cwd>/opencode.json                     yes
gemini            <cwd>/.gemini/settings.json             yes (+ mkdir)
codex             argv `-c mcp_servers.roy-connections.*` no  ← simplest
pi                — (unsupported by design)               n/a
```

## Open questions

- **Quoting edge cases.** Bundle paths today are `/var/folders/.../roy-mcp-bundle-<uuid>.json`. They never contain `"`. The defensive `replace('"', "\\\"")` handles future paths safely. **Decision:** keep the escape.
- **What if codex changes the `-c` parser?** Pinned to current codex-acp behavior. If `CliConfigOverrides` changes shape in a future release, this would need to follow. Same risk as every other preset writer that depends on the CLI's read schema.
