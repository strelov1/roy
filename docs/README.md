# docs/

Reference documentation for `roy`. Start at the top-level
[`README.md`](../README.md) for the elevator pitch and quick starts; the
files here go deeper.

| document                                       | scope                                                                 |
|------------------------------------------------|------------------------------------------------------------------------|
| [`architecture.md`](./architecture.md)         | layering and component responsibilities (transport, engine, manager, daemon, triggers, tests) |
| [`wire-protocol.md`](./wire-protocol.md)       | the single JSON shape used on CLI stdout, in the journal, and across every trigger |
| [`persistence.md`](./persistence.md)           | journal + metadata files, the two ids (roy-side vs agent-side), resume flow, idle GC |

Historical iteration notes are not preserved — git log is the
authoritative record of how things got to their current shape.
