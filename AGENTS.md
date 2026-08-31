# AGENTS.md — pgNative

> **pgNative is a native, fast, developer-first PostgreSQL client.**
>
> It is not pgAdmin, DBeaver, DataGrip, or a universal database GUI.
> The product exists to make the common PostgreSQL developer loop dramatically faster:
>
> **open → connect → find → query → inspect → edit → export → leave**

This document is the operating contract for humans and coding agents working on pgNative.

If a proposed change conflicts with this document, **do not silently reinterpret the product. Stop, explain the conflict, and ask for an explicit decision.**

---

## 1. Product Thesis

pgNative targets the large class of PostgreSQL users who repeatedly need to:

- connect to a database;
- browse schemas/tables/views/functions;
- write and run SQL;
- inspect result sets;
- make a small, safe data edit;
- search previous queries;
- export data;
- disconnect and get back to work.

The primary product differentiator is not "Rust" or "native" as an end in itself.

The differentiator is:

> **A deliberately narrow PostgreSQL client whose entire architecture and UX are optimized for speed, responsiveness, and low friction.**

Every feature should strengthen that loop.

If a feature does not materially improve the target workflow, it is presumed out of scope.

---

# 2. Non-Negotiable Product Constraints

These constraints are permanent unless the product owner explicitly changes them.

## 2.1 PostgreSQL only

pgNative supports PostgreSQL.

Do not add:

- MySQL
- MariaDB
- SQLite as a user-facing database
- MongoDB
- Redis
- MSSQL
- Oracle
- generic JDBC/ODBC database support
- "database adapters" intended to make pgNative universal

SQLite is permitted **internally** for local application storage such as history and preferences. It is not a supported user database.

## 2.2 Developer-first, not DBA-first

Do not turn pgNative into a PostgreSQL administration suite.

Explicitly out of scope unless separately approved:

- replication configuration
- tablespace management
- vacuum internals
- WAL administration
- logical replication management
- physical replication management
- advanced role/permission administration
- PL/pgSQL debugger
- server monitoring dashboards
- enterprise database fleet management
- backup management
- cloud database management
- Kubernetes database administration

A useful rule:

> If the feature is primarily for a PostgreSQL DBA rather than a developer interacting with application data, it probably does not belong in pgNative.

## 2.3 Local desktop application

v1 is a local desktop application.

Do not add:

- cloud synchronization
- hosted backend
- team workspaces
- collaboration
- shared queries
- server-side accounts
- mandatory telemetry infrastructure
- online project management
- browser-based application architecture

A local desktop application may communicate with the user's PostgreSQL server because that is the core function of the product.

## 2.4 No Electron

Do not introduce Electron.

Do not introduce Node.js as a runtime dependency.

Do not build the UI as a web application embedded in a desktop shell.

## 2.5 Native Rust application

The intended stack is:

- Rust
- `eframe`
- `egui`
- Tokio-based async execution
- PostgreSQL client libraries
- SQLite for local application state
- OS keychain for credentials

Do not introduce a second UI language/framework without explicit architectural approval.

---

# 3. Core UX Principle

The application should feel immediate.

Avoid interactions of this form:

> click → spinner → network request → rebuild UI → wait

Prefer:

> input → local state change → immediate UI response

Network/database operations should be asynchronous and isolated from the rendering loop.

The UI must remain responsive while:

- connecting;
- introspecting;
- executing queries;
- streaming results;
- exporting;
- cancelling queries;
- refreshing schema metadata.

---

# 4. Performance Is a Product Requirement

Performance is not an optimization phase.

It is a core product requirement.

## 4.1 Target budgets

Initial targets:

| Metric | Target |
|---|---:|
| Cold startup to interactive window | < 300 ms |
| Idle memory | < 80 MB |
| UI responsiveness during database I/O | No blocking |
| Query cancellation | Native PostgreSQL cancellation |
| Large result rendering | Virtualized |
| 500k+ rows | UI remains responsive |
| Result loading | Progressive, not full materialization |

These are targets, not excuses for unsafe or unreadable code.

Do not sacrifice correctness for benchmark numbers.

## 4.2 Benchmark definitions

Always distinguish:

### Cold startup

Process invocation → first interactive frame.

### Warm startup

Process invocation with relevant OS/filesystem caches already warm → first interactive frame.

### Connection readiness

User selects saved connection → database interaction/schema becomes usable.

### Query latency

Query submission → first usable result.

### Full result consumption

Query submission → requested result has been consumed.

Never report one as another.

## 4.3 Benchmark methodology

Performance claims must be reproducible.

When adding benchmark numbers:

- record hardware;
- record OS;
- record PostgreSQL version;
- record database/data shape;
- record query;
- record network conditions;
- record application build mode;
- state whether caches were warm/cold;
- report methodology alongside numbers.

Never fabricate benchmarks.

Never compare pgNative against another client using materially different conditions and present the result as scientific evidence.

---

# 5. Architecture

The intended high-level architecture is:

```text
┌─────────────────────────────────────────────────────────────┐
│                         pgNative                            │
│                                                             │
│  ┌──────────────────── UI / egui ─────────────────────────┐ │
│  │ Explorer │ Editor │ Results │ Connections │ History    │ │
│  └─────────────────────────┬──────────────────────────────┘ │
│                            │                                 │
│                      UI state/events                         │
│                            │                                 │
│  ┌─────────────────────────▼──────────────────────────────┐ │
│  │                    Application Layer                   │ │
│  │       commands · state · orchestration · lifecycle     │ │
│  └───────────────┬──────────────┬───────────────┬─────────┘ │
│                  │              │               │           │
│  ┌───────────────▼───┐  ┌──────▼────────┐  ┌──▼─────────┐ │
│  │   DB / Execution  │  │ Schema Model  │  │  Storage   │ │
│  │                   │  │               │  │            │ │
│  │ connections       │  │ introspection │  │ SQLite     │ │
│  │ sessions           │  │ cache         │  │ keychain   │ │
│  │ queries            │  │ completion    │  │ history    │ │
│  │ cancellation       │  │ metadata      │  │ settings   │ │
│  └───────────┬───────┘  └───────────────┘  └────────────┘ │
│              │                                              │
│       PostgreSQL server                                     │
│                                                             │
│  ┌────────────────────────────────────────────────────────┐ │
│  │                     Result Engine                      │ │
│  │ stream → bounded storage → viewport → egui rendering  │ │
│  └────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

---

# 6. Workspace Structure

The preferred workspace structure is:

```text
pgnative/
├── Cargo.toml
├── Cargo.lock
├── AGENTS.md
├── README.md
├── LICENSE
│
├── crates/
│   ├── app/
│   ├── db/
│   │   ├── connection/
│   │   ├── execution/
│   │   ├── cancellation/
│   │   └── introspection/
│   │
│   ├── schema/
│   │   ├── model/
│   │   ├── cache/
│   │   └── completion/
│   │
│   ├── results/
│   │   ├── stream/
│   │   ├── store/
│   │   └── viewport/
│   │
│   ├── storage/
│   │   ├── history/
│   │   ├── connections/
│   │   └── keychain/
│   │
│   └── ui/
│       ├── editor/
│       ├── explorer/
│       ├── results/
│       ├── connections/
│       └── layout/
│
├── xtask/
│
├── tests/
│   ├── integration/
│   ├── fixtures/
│   └── postgres/
│
├── benches/
│
├── docs/
│   ├── architecture/
│   ├── benchmarks/
│   └── decisions/
│
└── scripts/
```

The exact crate decomposition may evolve.

The architectural boundaries matter more than the exact directory names.

---

# 7. Dependency Policy

Every dependency has a cost.

Before adding a crate, ask:

1. Does the standard library already solve this?
2. Is the dependency mature?
3. Is it actively maintained?
4. Does it materially reduce complexity?
5. Does it introduce a large transitive dependency tree?
6. Does it conflict with the zero-runtime-dependency goal?
7. Does it work on macOS, Windows, and Linux?
8. Does it increase binary size or startup cost meaningfully?

Prefer small, focused, mature crates.

Do not add dependencies simply because they are convenient.

Do not write hundreds of lines of bespoke infrastructure merely to avoid a small, high-quality dependency.

---

# 8. Async / egui Boundary

This is a critical architectural seam.

`egui` rendering is synchronous and frame-driven.

Database/network work must not run directly inside the UI frame.

Use a model conceptually similar to:

```text
UI thread
   │
   ├── submit command
   │
   ▼
command/event channel
   │
   ▼
Tokio task
   │
   ├── PostgreSQL I/O
   ├── schema introspection
   ├── result streaming
   └── cancellation
   │
   ▼
event/result channel
   │
   ▼
UI thread
   │
   └── update application state
```

## Rules

- Never perform blocking database I/O on the UI thread.
- Never call blocking filesystem APIs repeatedly during rendering.
- Never sleep/block the render loop.
- Do not hold locks across `.await` unless explicitly justified.
- Avoid sending huge payloads through UI channels.
- Prefer typed events over loosely structured messages.
- Keep cancellation state explicit.
- Make task ownership and shutdown behavior clear.

---

# 9. Database Connection Architecture

A desktop client does not automatically need a traditional web-service connection pool.

Prefer explicit, long-lived PostgreSQL sessions where connection affinity matters.

Potentially separate:

- foreground query session;
- metadata/introspection work;
- other background operations.

Do not introduce `deadpool-postgres` merely because connection pooling is conventional.

Pooling is justified only if measurements and actual application semantics benefit from it.

## Connection state

Model connection state explicitly.

At minimum:

```text
Disconnected
Connecting
Connected
Executing
Cancelling
Error
```

If reconnection is implemented later, define it explicitly rather than hiding it inside arbitrary retry loops.

---

# 10. PostgreSQL Query Execution

Queries must be:

- asynchronous;
- cancelable;
- observable;
- isolated from the UI;
- protected by result backpressure.

The execution abstraction should expose concepts such as:

```text
QueryId
ConnectionId
QueryText
ExecutionState
StartTime
FirstResultTime
RowsReceived
CancellationState
Error
```

Avoid coupling the UI directly to `tokio-postgres` types.

The UI should depend on pgNative's execution model, not on the database driver implementation.

---

# 11. Query Cancellation

Cancellation means **actually requesting PostgreSQL cancellation**, not merely dropping a Rust future.

The conceptual flow is:

```text
User presses Cancel
        │
        ▼
Query Controller
        │
        ├── stop local consumption
        │
        └── send PostgreSQL CancelRequest
                    │
                    ▼
              PostgreSQL backend
```

Account for:

- cancellation after completion;
- cancellation during result streaming;
- network failure;
- transaction state;
- connection closure;
- cancellation races.

Cancellation must leave the connection in a known state.

Never silently reuse a connection whose protocol state may be corrupted.

---

# 12. Schema Introspection

Schema metadata should be treated as a first-class local model.

On connection:

1. establish session;
2. introspect required metadata;
3. construct `SchemaModel`;
4. populate local indexes;
5. make UI usable progressively.

Do not query PostgreSQL on every keystroke to populate autocomplete.

Do not rebuild the entire schema model unnecessarily.

## Schema model should support

- schemas;
- tables;
- views;
- functions;
- columns;
- types;
- primary keys;
- foreign keys;
- relevant constraints;
- generated/identity information where required for safe editing.

The same canonical schema model should power:

- schema tree;
- autocomplete;
- table metadata;
- editing safety;
- future explain/query features.

---

# 13. Schema Refresh

Schema refresh should be explicit or intelligently triggered.

Do not constantly poll.

Preferred behavior:

```text
Connect
  ↓
Initial introspection
  ↓
Cache
  ↓
Normal local usage
  ↓
Explicit refresh / relevant invalidation
  ↓
Incremental or full refresh
```

If a query creates a table or modifies schema, future schema invalidation may be triggered deliberately.

Do not make every query trigger full introspection.

---

# 14. SQL Editor

The editor is a core product surface.

MVP capabilities:

- multiple tabs;
- syntax highlighting;
- cursor/selection;
- basic SQL editing;
- execute current statement;
- execute selection;
- query cancellation;
- schema-aware completion;
- query history integration.

The editor must not block while executing.

## Completion

Basic completion should support:

- schemas;
- tables;
- columns;
- aliases;
- common PostgreSQL functions;
- schema-qualified identifiers.

Example:

```sql
SELECT u.
FROM users u
```

should be able to offer columns from `users`.

Do not attempt to recreate DataGrip's entire SQL intelligence stack in v1.

Do not write a full SQL parser unless there is a demonstrated need and a mature parser cannot be used.

Prefer a deliberately constrained implementation that is fast and predictable.

---

# 15. Result Engine

The result engine is one of the most important components in pgNative.

It must not equate:

> result set = Vec<Row>

That approach is unacceptable for large results.

Use a pipeline conceptually similar to:

```text
PostgreSQL
    │
    ▼
Result Stream
    │
    ▼
Bounded Result Store
    │
    ▼
Viewport
    │
    ▼
Virtualized egui grid
```

## Requirements

- progressive consumption;
- bounded memory;
- backpressure;
- viewport-based rendering;
- cancellation;
- responsive scrolling;
- handling of large text/JSON values;
- no full result materialization merely for display.

The result engine must be independent from egui as much as practical.

---

# 16. Large Result Strategy

Do not promise that every query can support arbitrary random access to arbitrary rows without cost.

SQL result sets do not inherently provide cheap random access.

Possible strategies include:

- bounded in-memory windows;
- server-side cursors;
- progressive streaming;
- spill-to-disk;
- explicit pagination for table browsing;
- query rewriting only where semantically safe.

Do not silently rewrite arbitrary SQL to add `LIMIT`, `OFFSET`, or ordering.

Do not change query semantics merely to make the UI convenient.

If random access is expensive, design the UX honestly.

---

# 17. Table Browsing vs Query Results

These are different workloads.

### Query results

User executed arbitrary SQL.

Properties:

- arbitrary ordering;
- arbitrary joins;
- arbitrary projections;
- arbitrary expressions;
- potentially huge streams.

### Table browser

User selected a table from the schema tree.

This can use a purpose-built browsing query such as:

```sql
SELECT ...
FROM ...
LIMIT ...
```

and can potentially support deliberate pagination strategies.

Do not treat both workloads as the same subsystem.

---

# 18. Result Rendering

The renderer should only create widgets for visible cells/rows plus a small overscan window.

Never create an egui widget for every row in a 500k-row result.

Avoid allocating large temporary structures every frame.

Avoid repeated string formatting for unchanged cells.

Use stable row/column identities where possible.

Measure:

- frame time;
- allocations;
- visible-row count;
- scroll performance;
- memory growth.

---

# 19. Data Representation

PostgreSQL values can be large and heterogeneous.

Do not blindly convert every value into a `String` immediately.

Where practical, preserve enough type information to distinguish:

- NULL;
- booleans;
- integers;
- floats;
- numeric;
- text;
- timestamps;
- UUIDs;
- JSON/JSONB;
- arrays;
- bytea;
- enums;
- other PostgreSQL types.

Rendering can choose an appropriate representation.

Large values should not be duplicated unnecessarily.

---

# 20. Inline Editing

Inline editing is a safety-sensitive feature.

Editing should be allowed only when pgNative can identify a row safely.

Preferred update model:

```text
Original row
     │
     ▼
Identify row using primary/unique key
     │
     ▼
Compute changed columns
     │
     ▼
Generate parameterized UPDATE
     │
     ▼
Show diff
     │
     ▼
Explicit confirmation
     │
     ▼
Execute
     │
     ▼
RETURNING *
     │
     ▼
Update displayed row
```

## Editing must be disabled when identity is unsafe

Examples:

- no primary key;
- ambiguous identity;
- unsupported relation type;
- view without a safe update model;
- insufficient metadata to generate a safe update.

Do not "guess" a row identity using every visible column.

## Never generate unsafe SQL

Values must be parameterized.

Never concatenate user data directly into SQL.

---

# 21. Concurrent Modification

If feasible, editing should detect obvious stale-row situations.

At minimum, avoid blindly claiming success.

A future optimistic-concurrency strategy may use original values or row identity where appropriate.

If the row changed between read and update:

- detect it when possible;
- do not silently overwrite unrelated changes;
- communicate the conflict clearly.

---

# 22. Transactions

Transaction state must be visible.

If a connection is inside a transaction:

```text
Transaction active
```

must be obvious in the UI.

Never make users guess whether:

```sql
BEGIN;
```

is still active.

Before closing/disconnecting a connection with an active transaction, provide an explicit decision where necessary:

- commit;
- rollback;
- cancel/keep open.

Do not silently commit user data.

Do not silently discard user data.

---

# 23. Query History

History is local.

Store:

- query text;
- timestamp;
- connection identifier/metadata sufficient for display;
- execution metadata where useful;
- success/failure where useful.

Do not store secrets.

Do not store plaintext credentials.

History should be searchable locally.

Do not upload query history to a remote service.

---

# 24. Credentials and Secrets

Credentials are security-sensitive.

Passwords/secrets belong in the OS keychain.

SQLite may store:

- host;
- port;
- database;
- username;
- display name;
- SSL configuration;
- non-secret SSH configuration.

SQLite must not store plaintext passwords.

## Never leak secrets into

- logs;
- debug output;
- panic messages;
- telemetry;
- crash reports;
- query history;
- clipboard helpers;
- benchmark output;
- screenshots generated by tests.

Be especially careful with connection URLs because they may contain passwords.

Sanitize them before logging or displaying.

---

# 25. SSH Tunnels

SSH tunneling is part of the connection experience but should remain isolated from PostgreSQL semantics.

Represent:

```text
Postgres connection
+
SSH tunnel configuration
```

as separate concerns.

Do not leak SSH credentials into PostgreSQL connection strings or generic logging.

Cross-platform SSH behavior must be tested explicitly.

---

# 26. SSL

Support explicit PostgreSQL SSL modes required by the MVP.

Do not silently weaken certificate verification to "make it work."

If an insecure option exists, it must be deliberate and clearly represented.

Never default to insecure behavior merely because development servers use self-signed certificates.

---

# 27. Storage

Use SQLite for local application state.

Expected categories:

```text
connections
history
preferences
editor state
```

Schema migrations must be versioned.

Do not modify production schema assumptions without migration handling.

Storage code should be testable without the UI.

---

# 28. Export

MVP export formats:

- CSV
- JSON
- SQL INSERT statements

Exports must not freeze the UI.

Large exports should stream where practical.

Do not load an entire 2GB result into memory merely to produce a CSV.

CSV correctness matters:

- quoting;
- embedded commas;
- quotes;
- newlines;
- NULL handling;
- encoding.

JSON correctness matters:

- NULL;
- numeric values;
- timestamps;
- binary/unsupported types.

SQL INSERT output must correctly escape/parameterize according to the intended output format.

---

# 29. UI Architecture

Avoid putting business logic directly inside egui rendering functions.

Bad:

```rust
ui.button("Run").clicked() {
    // connect
    // query
    // mutate database
    // update history
}
```

Prefer:

```text
UI event
  ↓
Application command
  ↓
Service/domain logic
  ↓
Async task
  ↓
Application event
  ↓
UI state update
```

This makes the application testable and prevents the UI layer from becoming the architecture.

---

# 30. egui Rendering Rules

Rendering should be:

- deterministic;
- cheap;
- side-effect-light;
- driven by application state.

Do not:

- execute SQL from render code;
- perform blocking filesystem work from render code;
- perform network requests from render code;
- repeatedly rebuild large collections unnecessarily;
- allocate massive temporary objects each frame.

If something is expensive, move it out of the render loop.

---

# 31. Visual Design

egui's default appearance is not the product's final visual identity.

A deliberate design system is required.

Define:

- typography;
- spacing scale;
- panel hierarchy;
- border/radius rules;
- selected/hover states;
- semantic status states;
- light/dark themes;
- editor styling;
- table density;
- iconography.

The goal is not "make egui look like a web app."

The goal is:

> **dense enough for developers, calm enough for daily use, and visually intentional.**

Avoid gratuitous animations.

Avoid decorative UI that adds latency or visual noise.

---

# 32. Keyboard-First UX

A developer database client must be usable primarily from the keyboard.

Important actions should have shortcuts for:

- new query tab;
- execute query;
- execute selection;
- cancel query;
- focus schema search;
- focus editor;
- search history;
- refresh schema;
- close tab;
- next/previous tab.

Shortcuts must be documented and should not conflict unnecessarily with platform conventions.

---

# 33. Error Handling

Errors should be:

- actionable;
- concise;
- technically accurate;
- associated with the relevant operation.

Do not replace useful PostgreSQL errors with:

> "Something went wrong."

Preserve useful information such as:

- SQLSTATE;
- server error message;
- detail;
- hint;
- position;
- context where safe.

Never expose credentials or connection secrets in error output.

---

# 34. Error Boundaries

A failed:

- query;
- export;
- schema refresh;
- connection;
- background task

must not crash the application.

Unexpected programmer bugs should still panic where appropriate during development rather than being swallowed.

Do not use broad `catch-all` error handling to hide defects.

---

# 35. Logging

Logging must be structured and useful.

Never log:

- passwords;
- connection URLs containing passwords;
- SSH private keys;
- secret tokens;
- arbitrary query parameters containing secrets.

Queries themselves may contain secrets, for example:

```sql
INSERT INTO users(email, password) VALUES (...)
```

Therefore query logging must be carefully controlled.

Default production logging should not dump arbitrary SQL text.

---

# 36. Testing Strategy

Testing is required at several levels.

## 36.1 Unit tests

Test:

- schema model;
- completion;
- SQL statement selection;
- result storage;
- row identity;
- diff generation;
- export encoding;
- connection configuration;
- migrations;
- state transitions.

## 36.2 Integration tests

Use real PostgreSQL where database behavior matters.

Test:

- connection;
- SSL;
- cancellation;
- schema introspection;
- large result streaming;
- edits;
- transactions;
- exports.

Do not mock PostgreSQL for behavior that depends on PostgreSQL semantics.

## 36.3 UI tests

Where practical, test:

- command routing;
- state transitions;
- tab behavior;
- connection lifecycle;
- query lifecycle;
- cancellation state;
- editing confirmation.

## 36.4 Performance tests

Have reproducible fixtures for:

- small result;
- 10k rows;
- 100k rows;
- 500k rows;
- 1m+ rows;
- wide rows;
- large text;
- JSON/JSONB;
- mixed PostgreSQL types.

---

# 37. PostgreSQL Test Fixtures

Maintain deterministic database fixtures.

Fixtures should include:

- normal tables;
- composite primary keys;
- foreign keys;
- views;
- functions;
- nullable fields;
- generated columns;
- identity columns;
- JSONB;
- arrays;
- timestamps;
- UUIDs;
- large text;
- tables without primary keys;
- large datasets.

Include intentionally awkward schemas.

The goal is to break the application before users do.

---

# 38. Performance Regression Policy

Every significant change to:

- result rendering;
- schema indexing;
- query execution;
- startup;
- application initialization;
- storage;
- editor completion

should be evaluated for performance regressions.

If a change makes startup 100ms slower, memory 40MB higher, or large-result scrolling materially worse, do not dismiss it without investigation.

Performance regressions require an explicit tradeoff.

---

# 39. Startup Discipline

Keep the startup path minimal.

Do not initialize expensive systems before the first interactive frame unless necessary.

Potential startup sequence:

```text
process start
    ↓
load minimal config
    ↓
initialize UI
    ↓
first interactive frame
    ↓
background initialization
    ↓
load local state
    ↓
ready
```

Measure rather than speculate.

Do not eagerly:

- connect to databases;
- fully introspect schemas;
- scan huge history tables;
- initialize unused features;
- perform network requests.

---

# 40. Feature Development Order

The intended progression is:

## Phase 0 — Validation

Validate:

- real current user pain;
- competitive gaps;
- egui/Tokio/Postgres architecture;
- startup feasibility;
- connection feasibility.

Deliverable:

> Technical spike + validated problem statement.

Kill the project if no meaningful, current, unaddressed pain is found.

---

## Phase 1 — Core Loop

Build:

- saved connections;
- OS keychain;
- schema tree;
- lazy loading;
- SQL editor;
- tabs;
- query execution;
- basic results;
- basic autocomplete.

Goal:

> Use pgNative for normal personal PostgreSQL work.

---

## Phase 2 — Differentiation

Build:

- result streaming;
- virtualization;
- bounded result storage;
- schema-aware completion;
- safe inline editing;
- query history;
- CSV/JSON export.

This is the most important engineering phase.

Goal:

> Prove that pgNative is materially faster and less annoying for the target workflow.

---

## Phase 3 — Polish

Build:

- visual design pass;
- cross-platform packaging;
- signing;
- notarization;
- onboarding;
- documentation;
- benchmark infrastructure.

Goal:

> Make the application trustworthy and pleasant to install/use.

---

## Phase 4 — Launch

Deliver:

- public releases;
- benchmark page;
- GitHub repository;
- landing page;
- installation instructions;
- issue tracker;
- community feedback loop.

Goal:

> Determine whether external developers independently choose pgNative over their existing client.

---

# 41. MVP Priority

If engineering capacity becomes constrained, use this priority order:

```text
P0 — Core
    connect
    query
    inspect
    responsive UI

P0 — Performance
    async execution
    cancellation
    result virtualization
    bounded memory

P1 — Developer UX
    schema explorer
    autocomplete
    history
    tabs

P1 — Safety
    keychain
    safe editing
    transaction visibility

P2 — Export
    CSV
    JSON
    SQL INSERT

P2 — Polish
    themes
    onboarding
    packaging

P3 — Convenience
    advanced editor intelligence
    Explain
    additional quality-of-life features
```

Never allow a P2/P3 feature to delay a broken P0.

---

# 42. Explicitly Cut These Features When Scope Explodes

If schedule slips, cut in this order:

1. advanced autocomplete intelligence;
2. JSON/SQL export extras;
3. sophisticated onboarding;
4. Explain;
5. advanced editor features;
6. cosmetic customization.

Do **not** cut:

- responsiveness;
- result virtualization;
- cancellation;
- credential security;
- safe editing semantics;
- core connection/query workflow.

---

# 43. What Not to Build

Unless the product owner explicitly changes scope, do not implement:

### Database breadth

- MySQL support
- SQLite user-database support
- MongoDB
- generic DB abstraction

### DBA tooling

- replication manager
- vacuum manager
- tablespace manager
- role administration suite
- server monitoring
- backup manager
- HA configuration

### Collaboration

- accounts
- teams
- shared queries
- cloud history
- comments
- real-time collaboration

### Platform bloat

- Electron
- embedded Chromium
- Node runtime
- browser-based UI

### Premature enterprise features

- SSO
- SCIM
- organization management
- centralized policy
- audit SaaS
- fleet management

If these appear in an issue or pull request, treat them as product-scope questions, not implementation tasks.

---

# 44. Security Principles

Security-sensitive code should favor explicitness over convenience.

Never:

- disable TLS verification by default;
- store passwords plaintext;
- interpolate user data into SQL;
- expose credentials in logs;
- execute arbitrary shell commands from connection settings without strict controls;
- silently modify user SQL;
- silently commit transactions.

Treat:

- SQL;
- connection strings;
- SSH configuration;
- exported data;
- database errors

as potentially sensitive.

---

# 45. Supply Chain and Build Reproducibility

Prefer:

- locked dependency versions;
- `Cargo.lock` committed for the application;
- reproducible release builds where practical;
- minimal dependencies;
- automated vulnerability/dependency checks;
- signed release artifacts.

Do not introduce an external binary dependency without documenting:

- why it is required;
- how it is obtained;
- licensing implications;
- cross-platform implications.

---

# 46. Cross-Platform Support

Target:

- macOS;
- Windows;
- Linux.

Do not assume filesystem, keychain, font, shell, SSH, or path behavior is identical across platforms.

Platform-specific behavior should live behind explicit abstractions.

Test each release on all supported platforms.

---

# 47. Packaging

Release artifacts should be installable by normal developers.

Target forms may include:

```text
macOS
    .dmg / signed application

Windows
    installer / signed executable

Linux
    AppImage or equivalent
```

The exact packaging mechanism can evolve.

The requirement is:

> A developer should not need Rust installed to use pgNative.

---

# 48. macOS Signing and Notarization

macOS distribution requires trust.

Release builds should be:

- code signed;
- notarized;
- packaged correctly.

Do not treat this as optional polish.

An unsigned application that triggers security warnings is a distribution failure.

---

# 49. Git Workflow

Keep commits small and coherent.

Good:

```text
feat(results): add bounded result store
fix(db): preserve connection affinity during transactions
perf(ui): avoid rebuilding result rows each frame
test(schema): add composite-key fixture
```

Avoid:

```text
"misc fixes"
"stuff"
"changes"
"final"
```

Do not mix unrelated refactors with feature work unless necessary.

---

# 50. Pull Request Requirements

Every non-trivial change should explain:

- what changed;
- why;
- affected architecture;
- performance impact;
- security implications;
- tests added/run;
- known limitations.

For UI changes, include screenshots or a short recording when useful.

For performance changes, include before/after measurements.

For database changes, include relevant PostgreSQL fixtures/tests.

---

# 51. Coding Standards

Prefer idiomatic Rust.

Use:

- `cargo fmt`;
- `cargo clippy`;
- `cargo test`.

Avoid:

- unnecessary `unsafe`;
- giant modules;
- global mutable state;
- hidden background tasks;
- unexplained `unwrap()` in production paths;
- excessive cloning;
- unbounded channels;
- unbounded collections fed by PostgreSQL.

`unwrap()` may be appropriate when an invariant is truly impossible to violate, but document that reasoning where it matters.

---

# 52. Ownership and Concurrency

Concurrency must have clear ownership.

For every background task, know:

- who starts it;
- who owns it;
- how it reports results;
- how it is cancelled;
- what happens when the window closes;
- what happens when its connection disappears.

Do not spawn detached tasks without a lifecycle strategy.

Prefer structured concurrency concepts where practical.

---

# 53. State Machines Over Boolean Soup

Avoid state representations such as:

```text
is_connecting
is_connected
is_loading
is_error
is_cancelling
```

where invalid combinations are possible.

Prefer explicit enums/state machines where the lifecycle has meaningful mutually exclusive states.

Example:

```rust
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error(...),
}
```

The same principle applies to query execution and editing.

---

# 54. UI State vs Domain State

Keep these distinct.

### Domain state

Examples:

- connection state;
- schema;
- query execution;
- result store;
- transaction state.

### UI state

Examples:

- selected tree node;
- active tab;
- splitter position;
- focused widget;
- scroll position;
- expanded nodes.

Do not contaminate database/domain models with arbitrary UI state.

---

# 55. Observability

Even if pgNative has no telemetry, local diagnostics should be useful.

Useful metrics internally include:

- startup duration;
- schema introspection duration;
- query start/first-result/completion;
- rows received;
- bytes received;
- cancellation latency;
- result-store memory;
- frame time.

Expose detailed diagnostics in development/debug builds where useful.

Do not send them anywhere by default.

---

# 56. Privacy

The default product should be privacy-respecting.

No mandatory cloud service.

No mandatory account.

No automatic upload of:

- query history;
- schema metadata;
- connection information;
- database contents;
- SQL statements.

If telemetry is ever proposed, it must be treated as a separate product/privacy decision.

---

# 57. Documentation

Documentation should prioritize:

1. installation;
2. first connection;
3. keyboard shortcuts;
4. query execution;
5. editing safety;
6. large result behavior;
7. troubleshooting;
8. architecture for contributors.

Do not write giant documentation for features that do not exist.

---

# 58. Architecture Decision Records

Significant architectural decisions should be documented in:

```text
docs/decisions/
```

Examples:

- why egui;
- why no Tauri;
- connection/session model;
- result storage strategy;
- cancellation architecture;
- keychain strategy;
- schema cache strategy.

An ADR should explain:

```text
Context
Decision
Alternatives
Tradeoffs
Consequences
```

Do not document decisions merely for ceremony.

Document decisions that future contributors might reasonably question.

---

# 59. Agent Workflow

When an agent receives a task:

## Step 1 — Understand

Read:

- this `AGENTS.md`;
- relevant crate/module documentation;
- related tests;
- relevant ADRs.

## Step 2 — Locate

Find the smallest correct architectural boundary for the change.

Do not immediately edit the first file containing the relevant string.

## Step 3 — Plan

For non-trivial changes, identify:

- state changes;
- concurrency implications;
- database implications;
- performance implications;
- security implications;
- tests.

## Step 4 — Implement

Prefer the smallest correct change.

Do not perform unrelated refactors.

## Step 5 — Test

Run the narrowest relevant tests first, then broader checks.

Typical sequence:

```text
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```

Use more targeted commands during iteration.

## Step 6 — Inspect

Check for:

- accidental blocking;
- unbounded memory;
- leaked secrets;
- unnecessary allocations;
- incorrect transaction behavior;
- cancellation races;
- cross-platform assumptions.

## Step 7 — Report

Summarize:

- what changed;
- tests run;
- benchmark impact if relevant;
- known limitations.

---

# 60. Agent Stop Conditions

An agent must stop and ask for clarification when:

- requirements conflict;
- implementing the feature would violate a non-goal;
- a security-sensitive behavior is ambiguous;
- destructive database behavior is unspecified;
- a major architectural boundary must change;
- a dependency with meaningful long-term cost is required;
- a performance target would be knowingly compromised;
- the correct behavior depends on an unresolved product decision.

Do not make product decisions disguised as implementation details.

---

# 61. Database Safety During Development

Never run destructive commands against an unknown production database.

When writing integration tests:

- use isolated databases;
- use deterministic fixtures;
- clean up after tests;
- make destructive setup explicit.

Never assume:

```text
localhost = safe
```

A developer's localhost may point to production-like infrastructure.

Tests should make their target explicit.

---

# 62. Migration Safety

Application-local SQLite migrations must be:

- versioned;
- deterministic;
- tested;
- backward-aware where required.

Never silently destroy user history or saved connection metadata during an upgrade.

---

# 63. Accessibility and Usability

The application is developer-oriented, but accessibility still matters.

Ensure:

- readable text;
- sufficient contrast;
- keyboard operation;
- visible focus;
- non-color-only status communication;
- scalable UI where practical.

Do not rely solely on tiny icons for important actions.

---

# 64. Internationalization

Do not prematurely build a full localization framework.

However:

- avoid hardcoding text in ways that make future localization impossible;
- avoid assumptions that all text has fixed width;
- avoid truncating critical error messages solely because English strings are short.

Localization is not an MVP requirement.

---

# 65. Telemetry and Analytics

Default:

> No remote analytics required.

If analytics are ever considered, evaluate whether they materially improve product decisions.

Prefer anonymous, opt-in, minimal telemetry if ever introduced.

Never send database contents or SQL text by default.

---

# 66. Release Quality Gate

A release is not ready merely because it compiles.

Before public release:

### Functional

- connection works;
- schema explorer works;
- queries work;
- cancellation works;
- results work;
- editing is safe;
- exports work;
- history works.

### Performance

- startup measured;
- idle memory measured;
- large-result behavior measured;
- no obvious UI blocking.

### Security

- credentials in OS keychain;
- no secrets in logs;
- TLS behavior verified;
- SQL values parameterized.

### Packaging

- macOS package signed/notarized;
- Windows package tested;
- Linux package tested.

### Documentation

- installation;
- usage;
- known limitations;
- benchmark methodology.

---

# 67. Definition of Done — Six-Month Product

At the end of the initial build period, pgNative should be:

> **A downloadable, signed, cross-platform native PostgreSQL desktop client that a developer can use for their normal daily PostgreSQL workflow without opening pgAdmin.**

Specifically:

```text
Connect
  ↓
Find a table
  ↓
Write SQL
  ↓
Autocomplete
  ↓
Execute asynchronously
  ↓
Inspect large results
  ↓
Scroll without UI collapse
  ↓
Edit a safe row
  ↓
Review generated diff
  ↓
Confirm update
  ↓
Export if needed
  ↓
Search query history
```

The application should have:

- real users;
- reproducible benchmarks;
- public releases;
- a credible landing page;
- a public issue tracker;
- documented architecture;
- measurable performance characteristics.

It does **not** need:

- feature parity with pgAdmin;
- DBA administration features;
- multi-database support;
- collaboration;
- cloud infrastructure;
- enterprise management.

---

# 68. The Product's Ultimate Test

The strongest validation is behavioral.

Not:

> "Does pgNative have enough features?"

Not:

> "Does the README look impressive?"

Not:

> "Is the architecture elegant?"

The real test is:

> **When a developer needs to inspect or modify a PostgreSQL database, do they instinctively open pgNative instead of their old database client?**

If yes, the product is working.

If no, find the friction.

Classify every failure as:

```text
Missing capability
UX friction
Performance
Reliability
Safety
Habit
```

Then fix the highest-frequency causes.

Do not respond to every piece of feedback by adding a feature.

---

# 69. Product Discipline

When evaluating a new feature, ask:

### Does it improve the core loop?

```text
open
connect
find
query
inspect
edit
export
leave
```

### Does it make pgNative faster?

### Does it make pgNative safer?

### Does it remove repeated developer friction?

### Does it serve the target developer?

### Does it create significant maintenance cost?

### Does it push pgNative toward being a generic database tool?

If the answers are mostly negative:

> **Do not build it.**

---

# 70. Final Rule

pgNative wins by doing less.

It should not try to beat every database client at everything.

It should make one workflow exceptionally good:

```text
PostgreSQL
+
Developer
+
Fast
+
Native
+
Simple
```

The product must remain narrow enough that every millisecond, every click, and every interaction can be optimized around that workflow.

**Do not build pgAdmin #2.**
