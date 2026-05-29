# docs/

Reference documentation for `roy`. Start at the top-level
[`README.md`](../README.md) for the elevator pitch and quick starts; the
files here go deeper.

| document                                       | scope                                                                                                                |
|------------------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| [`architecture.md`](./architecture.md)         | layering and component responsibilities across the eight crates (core daemon, CLI, MCP, management, auth, scheduler, gateway, inbound) |
| [`wire-protocol.md`](./wire-protocol.md)       | the single JSON shape used on CLI stdout, in the journal, and across every trigger                                   |
| [`persistence.md`](./persistence.md)           | every SQLite file roy writes, every table, the two ids (roy-side vs agent-side), resume flow, idle GC                |
| [`harnesses-config.md`](./harnesses-config.md) | `~/.config/roy/harnesses.toml` — which ACP harnesses and models are surfaced to clients                              |
| [`examples/inbound.example.toml`](./examples/inbound.example.toml) | sample `roy-inbound` configuration                                                                |
