# pgNative — 4-Track Implementation Plan

**Workspace:** `/home/sachin/Desktop/Code/pgnative` — branch `phase0-validation` @ `510abdc`
**Date:** 2026-08-31
**Sources:** `AGENTS.md`, `Cargo.toml`, `crates/schema/model/src/{types,relation,column,index,build}.rs`, `crates/results/value/src/lib.rs`, 4 subagent planners (DB `01a05754-8eb1` + Schema `01a05754-9dd5` + Results `01a05754-ad0c` + App/Storage/UI `01a05754-bdb5`) — latter two arrived late (after initial synthesis) and have been folded in; see `subagent/*/session.jsonl`

## Goal

Deliver the P0/P1 MVP that proves the thesis in `AGENTS.md` §1 / §67:

> **open → connect → find → query → inspect → edit → export → leave**

…as a native Rust + egui + Tokio desktop app that is async, cancelable, virtualized, bounded-memory, and safe to edit — narrow enough that every interaction can be optimized for the PostgreSQL developer loop.

This plan covers **all 4 tracks in parallel** as requested, with one integrated execution order:

| Track | Scope (AGENTS.md) | Crates |
|---|---|---|
| **A — DB/Execution** | §9 Connection, §10 Execution, §11 Cancellation, §22 Transactions, §26 SSL, §25 SSH, §33 Errors | `crates/db/{connection,execution,cancellation,introspection}` |
| **B — Schema** | §12 Introspection, §13 Refresh, §14 Completion, §36 fixtures | `crates/schema/{model,cache,completion}` + `crates/db/introspection` |
| **C — Results** | §15 Result Engine, §16 Large Results, §17 Table vs Query, §18 Rendering, §19 Data Rep, §20-21 Editing, §28 Export | `crates/results/{value,stream,store,viewport,table_browser,edit,export}` |
| **D — App/Storage/UI** | §8 Async/egui boundary, §23 History, §24 Secrets, §27 Storage, §29-30 UI Arch, §31-32 Design/Keys | `crates/{app,storage/*,ui/*,observability}` |

---

## Success Criteria

### Product (from §41, §66-68)
1. A developer can download a signed binary (no Rust required), save a connection (password in OS keychain), connect, browse `public` tables via schema tree (1k relations <500ms to interactive), run `SELECT` in editor tabs, see first rows streaming, scroll 500k rows without UI collapse, cancel a long query with native PG cancellation, edit one safe row (PK-gated, parameterized `UPDATE … RETURNING *`, diff + confirm), export CSV/JSON, and find the query in local searchable history — then leave.
2. **Performance budgets (§4.1) met on reference hardware** (record hw/OS/PG version/query/network/build mode per §4.3): cold start <300ms, idle <80MB, no UI blocking during DB I/O, cancellation <500ms, viewport renders only visible rows + overscan.

### Technical (from §8, §53, §66)
3. `cargo fmt --check` + `cargo clippy --workspace --all-targets --all-features` green, `unsafe_code=deny` upheld, `cognitive-complexity-threshold = 30` (`clippy.toml`).
4. `cargo test --workspace` green including `testcontainers` integration for connection/cancellation/introspection/editing on PG 15/16; no secrets in logs (`tracing` sanitized); no `tokio-postgres` types leaking into `crates/ui`.
5. Single canonical `SchemaModel` powers tree + completion + `Editability`; `CellValue` preserves PG types (not eager `String`); connection state is an explicit enum, not boolean soup.

---

## Context And Current Facts

### What exists (2026-08-31)
* **Implemented:** `pgnative-schema-model` (`crates/schema/model/src/lib.rs:1-15` re-exports; `types.rs` defines `Id/Oid/RelationKind/ValueSource/Nullability/Editability`; `relation.rs:14-43` defines `Relation` with `primary_key: Option<PrimaryKey>` + `unique_keys` + `Editability` gating at `relation.rs:32`; `column.rs:8-22` defines `Column`; `schema.rs:7-15` defines `Schema`; `index.rs:18-42` defines `SchemaModel` with `schema_by_name/relation_by_oid/relations_by_schema/sorted_relations/column_owner` indexes; `build.rs:13-78` defines `Builder` with dense `Id = vec.len()` allocation; `tests.rs:1-102` smoke tests). `pgnative-results-value` (`crates/results/value/src/lib.rs:1-132`) defines `CellValue` (19 variants, `Bytes`-backed large values, `is_null/is_textual/to_display_string`) and `Row { index, cells }`.
* **Stubs (empty dirs, no Cargo.toml):** `crates/db/{connection,execution,cancellation,introspection}`, `crates/schema/{cache,completion}`, `crates/results/{stream,store,viewport,table_browser,edit,export}`, `crates/storage/{connections,history,keychain,preferences,editor_state}`, `crates/ui/{editor,explorer,results,connections,layout,theme,shortcuts,history_panel}`, `crates/app`, `crates/observability`, `tests/integration`, `benches`, `docs/decisions` (empty).
* **Workspace manifest (`Cargo.toml`):** members = `[crates/app, crates/db/connection, crates/db/execution, crates/results/value, crates/schema/model]` — `cancellation/introspection/cache/completion/store/stream/viewport/...` **not yet members** (must be added). Dependencies pinned: `eframe/egui 0.36.1`, `tokio 1.53 (rt-multi-thread, sync, net, io-util)`, `tokio-postgres 0.7.18 + tokio-postgres-rustls 0.14 + rustls 0.23 (ring,tls12)`, `postgres-types/protocol`, `rusqlite 0.40 (bundled)`, `keyring 4.2 + secrecy 0.10 + zeroize 1.9`, `chrono/uuid/serde_json/bigdecimal/bytes/arcstr/smol_str`, `crossbeam-channel 0.5 + parking_lot 0.12`, `tracing 0.1 + tracing-subscriber`, `testcontainers 0.28`.
* **Toolchain:** `rust-toolchain.toml: channel 1.96.0` with `clippy, rustfmt`.
* **Git:** single commit `510abdc Initial commit`, untracked `AGENTS.md, Cargo.toml, crates/, rust-toolchain.toml, rustfmt.toml, clippy.toml`.

### Implications
* All 4 subagent planners returned full drafts (DB `01a05754-8eb1`, Schema `01a05754-9dd5`, Results `01a05754-ad0c`, App/Storage/UI `01a05754-bdb5`) — incorporated. Results + App arrived ~90s late (after initial synthesis) and were merged into Key Decisions C/D and Recommended Approach without overwriting audited DB/Schema decisions.
* DB layer is greenfield behind already-decided `tokio-postgres` choice — do not introduce `sqlx`/`deadpool-postgres`.

---

## Constraints And Non-goals

### Hard constraints (AGENTS.md §2-4, §8, §11, §20, §24)
* **PostgreSQL-only** — no MySQL/MariaDB/Mongo/generic adapters; SQLite only for local app state.
* **Developer-first, not DBA** — no replication/WAL/vacuum/role-admin/PL/pgSQL debugger/monitoring.
* **Local desktop only** — no cloud sync, teams, hosted backend, collaboration, telemetry.
* **No Electron/Node** — Rust + `eframe`/`egui` + Tokio only.
* **Core UX:** `input → local state → immediate UI`, DB work off the render loop (`UI → command channel → Tokio → event channel → UI`); no blocking I/O/sleep/holding locks across `.await`.
* **Cancellation = real `CancelRequest`** on a separate connection (PID + secret_key), not future-drop; connection ends in defined `Ready|Error|ReconnectRequired`.
* **Editing only when safe** — require `Editability::EditableWithPrimaryKey | EditableWithUniqueKey` (unique where every column is `NotNull` per `relation.rs:78`); `GENERATED/IDENTITY/Virtual` columns per `types.rs:49` are never writable; generate parameterized `UPDATE … RETURNING *` + diff + explicit confirm.
* **Secrets:** passwords only in OS keychain (`keyring` + `secrecy`/`zeroize`), sanitize URLs before `tracing` (§35), never log query text with embedded secrets.
* **Result engine:** progressive, bounded-memory, backpressured, virtualized — never `Vec<Row>` for large results; never silently rewrite arbitrary SQL with `LIMIT/OFFSET`.
* **Benchmarks must be reproducible** per §4.3 — never fabricate or compare under different conditions.

### Non-goals for this plan (cut per §42 if scope slips)
* Advanced autocomplete intelligence beyond prefix/alias/schema-qualified + ~30 common PG functions; full DataGrip-level SQL intelligence deferred.
* JSON/SQL INSERT export extras beyond correct CSV/JSON null/quote/escape handling.
* Sophisticated onboarding, visual design polish beyond `ui/theme` tokens, Explain, advanced editor features — P2/P3.
* Experiments are not non-goals where they validate risk: do run `testcontainers` PG fixtures for behavior that depends on PG semantics (§36).

---

## Key Decisions

### A — DB/Execution (from subagent `01a05754-8eb1`)
| # | Decision | Chosen | Rejected & Why |
|---|---|---|---|
| A1 | **Driver** | Keep `tokio-postgres 0.7.18 + tokio-postgres-rustls 0.14 + rustls (ring,tls12)` (already pinned) | `sqlx` — hides `CancelToken`, cancellation = future-drop, `Pool` breaks affinity (`SET`, `BEGIN`, portals). `postgres` (sync) + `spawn_blocking` — blocks Tokio, still needs `tokio-postgres` for cancel. |
| A2 | **Connection model** | Explicit long-lived sessions: `ConnectionHandle { query_session: ManagedSession, meta_session: Option<ManagedSession> }` per `ConnectionId`; each `ManagedSession` wraps `Client + Connection driver task + CancelToken`; `meta_session` lazily created for introspection/table-browser | `deadpool-postgres/bb8/mobc` pool — 1–3 desktop connections, pooling breaks affinity (prepared statements, cursors, temp tables, `LISTEN`) and adds no win; revisit only with measurement + ADR |
| A3 | **State machine** | Enum, not bool soup (§53): `enum ConnectionState { Disconnected, Connecting{since}, Connected{conn_id, tx: TxState, health: SessionHealth}, Executing{conn_id, query_id, can_cancel}, Cancelling{query_id, sent_at}, Error{kind, retryable} }` + `TxState { Idle, InTransaction, InFailedTransaction }` mirroring `ReadyForQuery 'I'/'T'/'E'` + `SessionHealth { Ready, NeedsReset, Poisoned }` | `is_connecting/is_connected/is_cancelling` booleans — admits invalid `connecting && connected` |
| A4 | **Execution abstraction** | App-owned types (§10): `QueryId/ConnectionId(Uuid)`, `QueryRequest { conn, sql, params }`, `ExecutionState { Queued, Executing{start}, Streaming{first_row, rows}, Cancelling, Completed{rows,elapsed}, Failed(PgError), Cancelled }`, `PgError { sqlstate, message, detail, hint, position, is_cancel }`; flow `Command::Execute --crossbeam bounded(64)--> Tokio task (Client::query_raw) --> Row→CellValue via postgres-types codec --> Event::Rows (bounded 32) --> UI`; never expose `tokio_postgres::Row/Error` to `crates/ui` | Expose driver types to UI — coupling; `sqlx::query` pool path |
| A5 | **Cancellation** | Native: capture `client.cancel_token()` at connect; on Cancel, (1) stop local consumption, (2) spawn short-lived `TcpStream+rustls` via `cancel_token.cancel_query(...)`; server returns `57014 query_canceled`; map to `PgError { is_cancel:true }`; 3s timeout → `Poisoned`; handle: already-completed → no-op, network fail → `NeedsReset`, inside Tx → `InFailedTransaction` until `ROLLBACK`, closed → `NotConnected` (idempotent) | Future-drop only — leaves backend running (§11 violation) |
| A6 | **Transaction visibility** | Dual: optimistic classifier for `{BEGIN,START TRANSACTION,COMMIT,ROLLBACK,SAVEPOINT,RELEASE,ROLLBACK TO}` (case/whitespace/comment-insensitive) + authoritative `ReadyForQuery` status byte piggyback (`SHOW transaction_status` if hook unavailable); sticky badge `Transaction active` when `TxState != Idle`; on close/disconnect with `TxState != Idle` emit `DisconnectRequiresDecision { Commit, Rollback, KeepOpen }`, never auto-commit | Poll `pg_stat_activity` — stale/permissions; parser-only — misses `SET` |
| A7 | **Errors** | `thiserror` `DbError { ConnectionFailed, AuthFailed, TlsFailed, QueryFailed(PgError), CancelFailed, TransactionPoisoned }` preserving `SQLSTATE/detail/hint/position` (§33); `url::Url` sanitizer redacts password before `Display`/`tracing`; `anyhow` only in `xtask` | `anyhow` in public API — loses typed handling |
| A8 | **Secrets/SSL** | `ConnectionConfig { host, port, dbname, user, ssl_mode, ssl_root_cert, ssh_tunnel }` in SQLite; `password: SecretString (secrecy+zeroize)` in `keyring`; `Debug` redacts `***`; `SslMode` maps 1:1 to `tokio_postgres::config::SslMode { Disable, Prefer, Require, VerifyCa, VerifyFull }`, default `Prefer`, never silent downgrade (§26) | Plaintext SQLite password; `SslMode::Disable` default |

### B — Schema (from subagent `01a05754-9dd5`)
| # | Decision | Chosen | Rejected & Why |
|---|---|---|---|
| B1 | **Introspection source** | 5 qualified `pg_catalog` queries (schemas, relations `relkind IN ('r','v','m','f','p')`, columns+types via `pg_attribute/pg_type/pg_attrdef`, PK/unique via `pg_constraint/pg_index`, FKs, functions via `pg_proc`) | `information_schema` — loses OID/`attgenerated`/`attidentity`/FK OID; single giant join — couples failures |
| B2 | **Model identity** | Keep dense `Id(u32)` + separate `Oid(u32)` (`types.rs:11`); `Builder` allocates `Id = vec.len()` with `debug_assert_eq!` | OIDs as map keys — non-dense, gaps; UUIDs — meaningless |
| B3 | **Type storage** | Extend `types: HashMap<TypeId,String>` to `Type { name: SmolStr, category, is_array }` flat map; keep `Column.ty: TypeId` | Full `pg_type` normalization — 200 fields, overkill; `String` only — loses array/enum detection for export/edit guards |
| B4 | **Ordering** | Deterministic `(schema.name, relation.name)` — fix current `build.rs:81` which sorts `sorted_relations` by `name` alone; sort `relations_by_schema` buckets similarly | Unsorted — flaky tests/tree jitter; C collation — diverges from PG |
| B5 | **Cache store** | In-memory `Arc<SchemaModel>` behind `arc-swap 1.7` (or `parking_lot::RwLock<Arc<_>>` if crate budget denied); no SQLite persistence in v1 | SQLite mirror — migration risk, stale; `OnceLock` — no refresh |
| B6 | **Progressive loading** | Two-phase: A = schemas+relations+columns+types (tree + `alias.` completion); B = PKs/unique/FKs/functions/comments; swap `Arc` twice, render after A | All-or-nothing — blocked on `pg_proc` scan; per-schema lazy — N+1 + violates §12; `LISTEN/NOTIFY` DDL — superuser |
| B7 | **Refresh** | Explicit `Refresh` command + heuristic: after `ExecutionState::Success` if `command_tag ~ CREATE|ALTER|DROP`, mark `Stale` and banner; never auto-full-refresh per query; `u64` epoch + `relation_by_oid` diff | Poll loop (§13 ban); auto full-refresh per query — loses scroll state |
| B8 | **Connection affinity** | Dedicated read-only introspection session (`statement_timeout=5s`, `SET TRANSACTION READ ONLY`, qualified `pg_catalog`) | Reuse query session — entangles `Executing/Cancelling` |
| B9 | **Completion engine** | Local `crates/schema/completion`: `HashMap<lowercase, Vec<RelationId>>` + per-relation column `Vec<ColumnId>` + trigram/fuzzy; alias map from lightweight `FROM/JOIN … [AS] alias` regex + optional `sqlparser 0.51` feature; scores exact-prefix > schema-qualified > alias column > function > fuzzy; cap 50 items <1ms; include ~30 common PG functions | Per-keystroke DB `SELECT` — §12 ban; `libpg_query` FFI — native dep; LSP |
| B10 | **Parser** | Optional `sqlparser` behind feature; fallback hand-rolled `FROM\b|JOIN\b` + quoted-identifier splitter (`"My Table"` → `My Table`) | `pest`/`nom` bespoke grammar — 500-line bug compat; `pg_query` crate — libpg |

### C — Results (from subagent `01a05754-ad0c`, refined)
| # | Decision | Chosen | Rejected & Why |
|---|---|---|---|
| C1 | **Pipeline** | `Postgres --Row stream--> stream::ResultStream --tokio::mpsc bounded(512) StreamEvent--> store::BoundedStore --viewport window--> ui::results virtualized grid` per §15; `CellValue` stays `Bytes`-backed until render (§19); store independent of `egui`; `StreamEvent::{Meta, Batch(Vec<Row>), Error, Complete, Cancelled}` batches ≤256 rows | `result = Vec<Row>` — unbounded memory, collapses at 500k (§15) |
| C2 | **Stream→Store channel** | `tokio::sync::mpsc::bounded(512)` on Tokio side, bridged to `crossbeam` for UI `try_recv`; execution owns `CancelToken` + `StreamHandle::cancel()` forwards to `db/cancellation` (D11) | `crossbeam` unbounded — no backpressure; custom poll — extra runtime |
| C3 | **Store budget** | Both `row_budget (50k)` + `byte_budget (64 MiB)`, byte budget authoritative; `Row.index` stable (global insertion order, never rewritten on eviction → stable egui `Id`); `Row::byte_len()` for accounting; `StoreState::{Streaming, Complete, Error, Cancelled}` | Row-count only — wide JSON OOM; byte-only — narrow rows over-evicted |
| C4 | **Eviction / spill v1** | Ring-window (drop oldest) + truncation + block producer via channel backpressure; spill-to-temp-SQLite deferred to Phase 2; honest UX “Showing 50k of 500k — export/scroll to fetch” per §16 | Spill-to-disk now — extra fsync/recovery not P0 (§41) |
| C5 | **Server cursor** | Simple `query_raw` + streaming for arbitrary SQL v1; `DECLARE CURSOR + FETCH` inside explicit tx deferred to Phase 2 opt-in (>100k results) | Cursor v1 — requires tx pinning, conflicts with §22 visibility |
| C6 | **Table browser pagination** | Keyset `WHERE (pk) > $last ORDER BY pk LIMIT $n` when `Editability != Disabled`; fallback `LIMIT/OFFSET` with warning; `table_browser::build_sql()` emits parameterized SQL, `Err(NotBrowsable)` if `Disabled` — never guess | `OFFSET` only — degrades, skips on concurrent writes |
| C7 | **Viewport ownership / overscan** | UI owns `ViewportState { offset, len, overscan=10, row_height=22px, col_widths }`; store is `Arc<parking_lot::RwLock<ResultStore>>` mutated only by Tokio, read via `snapshot_range(offset,len)->Arc<[Row]>`; `visible_range(scroll_y, viewport_h)` with fixed 10-row + 2-col overscan | Store owns viewport — locks in render; dynamic overscan — unpredictable |
| C8 | **Type decoding / large values** | Text protocol v1 via `FromSql` for known OIDs + binary for int/float/uuid where driver supports; decoding lives in `stream` mapper `Oid→CellValue`; `per_cell_cap=256 KiB` in store, `render_cap=2 KiB` with “… (N bytes) — expand” affordance (§19) | Eager `to_string` per cell — alloc per frame |
| C9 | **Crate boundaries** | Keep `value` as published; add `stream/store/viewport/table_browser` as separate crates with minimal deps; `edit/export` depend on `value+store`, renderer only on `viewport+value` — enforces egui-independence via dep direction CI check | Monolith `results` — breaks §5 diagram |
| C10 | **Data rep** | Preserve `CellValue` enum exactly as `crates/results/value/src/lib.rs:22-54` (`Null…Other`) plus `ColumnMeta { name, pg_type: Oid, type_name, nullable }` + `Value::truncated_display(cap)` helper | Collapse to `String` — loses NULL/numeric/bytea |
| C11 | **Editing** | `crates/results/edit`: `identify(row)->Option<RowIdentity>` via `Editability`; `diff` filtering `Generated/Identity/Virtual`; `UPDATE … WHERE pk=$N RETURNING *` parameterized + optimistic `AND (col IS NOT DISTINCT FROM $orig)` guard → `Conflict::NotFound / Modified { current: Row }` dialog; disabled when `Disabled` | Guess via all visible columns — unsafe (§20); concat SQL — injection |
| C12 | **Export** | `crates/results/export`: `Exporter::export(Stream<Row>, &[ColMeta], AsyncWrite)->Stats` streaming 1k-row windows; CSV `csv` (RFC4180, NULL→empty), JSON NDJSON/array typed (`Null→null`, `Bytea→base64`, `Timestamp→RFC3339`), SQL `INSERT INTO "schema"."table" … VALUES` via `postgres-protocol` escaping batched 100 rows; backpressure-aware | Full materialize 2GB

### D — App/Storage/UI (from subagent `01a05754-bdb5`, refined)
| # | Decision | Chosen | Rejected & Why |
|---|---|---|---|
| D1 | **Async/egui seam (§8, §30)** | `crossbeam-channel::bounded(256)` `Ui→App` (`AppCommand`) polled via `try_recv` + `ctx.request_repaint()`, `tokio::sync::mpsc(256)` inside Tokio bridged to `crossbeam` egress (`AppEvent`); never send `Row/Bytes` batches — flow `stream→store→viewport` and UI pulls `Viewport` slice; `AppController` owns `JoinSet` + `AbortHandle`/`CancellationToken` (`Idle/Cancelling/Completed`), `Drop` aborts on window close then PG `CancelRequest`; no `MutexGuard` across `.await` — `parking_lot::RwLock<Arc<SchemaModel>>` for swap | `tokio::mpsc` directly on UI (needs `poll`), `async-channel` (extra dep), unbounded channels |
| D2 | **App orchestration (§29)** | `crates/app` owns `AppCommand {Connect,Disconnect,Execute{tab,sql,selection},Cancel,EditCommit,Export,HistorySearch,RefreshSchema}` + `AppEvent {ConnectionStateChanged,QueryProgress,QueryFinished,SchemaUpdated(Arc<SchemaModel>),ExportProgress,Error{op, sanitized PgError}}` + `AppState { connections: StateMachine, queries: Map<QueryId,ExecutionState>, schema: ArcSwap, tx: Map<ConnectionId,TxState> }` + `AppController { cmd_rx, event_tx, ui_egress, tasks: JoinSet }`; UI pure `fn show(ui:&mut Ui, ui_state:&mut UiState, app_state:&AppState, sink:&Sender<AppCommand>)`, never imports `tokio-postgres` | `String` events; business logic in `ui.show()` button |
| D3 | **Storage (§27)** | Single `pgnative.db` via `directories::ProjectDirs("com","pgnative","pgnative")` → `data_dir()/pgnative.db` (WAL, `foreign_keys=ON`, `busy_timeout=5000ms`); one `rusqlite::Connection` behind `parking_lot::Mutex` on `spawn_blocking` thread (UI never calls `rusqlite`); migrations table `{version, applied_at}` + `PRAGMA user_version` + `include_str!` SQL; tables `connections(id,…ssl/ssh non-secrets, no password)`, `history(id,connection_id,query_text 64KB truncated,executed_at,duration,rows,success,error_code)` + `FTS5 history_fts(query_text)` triggers, `preferences(key,value JSON)`, `editor_state(tab_id,connection_id,content,cursor,scroll)` | Multiple DB files; `rusqlite` on UI thread (blocks render) |
| D4 | **Secrets (§24)** | `storage/keychain` wraps `keyring` (`service=com.pgnative.pgnative, account=connection/<uuid>`), `Secrecy<SecretString>` + `ZeroizeOnDrop` in memory; `set/get/delete_password(conn_id)` → `KeychainError` (no secret in Display); fallback in-memory+warning if platform unsupported (never plaintext SQLite); helper `sanitize_url(&str)->String` for `tracing` | Env var / plaintext SQLite |
| D5 | **History (§23)** | Local only, searchable offline via `FTS5` (see D3); `truncate 64KB` before insert; no credential-pattern scan — rely on URL sanitization at write; prepare + `IMMEDIATE` transaction | Cloud sync / shared queries |
| D6 | **Editing safety (§20-21)** | Allow iff `Editability == EditableWithPrimaryKey | EditableWithUniqueKey`; flow `original Row + candidate → changed_columns = diff()` skipping `Generated/Identity/Virtual` → `DiffView` → confirm → parameterized `UPDATE {table} SET … WHERE pk=$N RETURNING *` via `postgres-types ToSql`; concurrent guard `WHERE pk=$N` check `rows_affected` (0→NotFound) or optional `AND (col IS NOT DISTINCT FROM $orig)` → `Conflict::Modified { current }` with Reload/Overwrite; `Bytes` refs, no duplication; `xmin` not used v1 | Guess via all visible cols; `xmin` system-col dep |
| D7 | **Transaction visibility (§22)** | `TxState {Idle, InTransaction{since,readonly}, FailedTransaction}` from PG `ReadyForQuery 'I'/'T'/'E'` via `tokio-postgres` notification (not string-matching `BEGIN`); UI bottom-bar badge amber “● Transaction active” / red `FailedTransaction`; disconnect/close with `InTransaction` → modal `Commit / Rollback / Keep Open` (default `Rollback`), never silent | String-matching only; `pg_stat_activity` poll |
| D8 | **Export streaming (§28)** | `crates/results/export` trait `Exporter::export(Stream<Row>, &[ColMeta], AsyncWrite)->Future<Stats>` from `ResultStore` snapshot (not channel payloads); `CsvExporter` RFC4180, `JsonExporter` NDJSON/array, `SqlExporter` `INSERT INTO "schema"."table" …` via `postgres-protocol::escape` batched 100 rows; Tokio task + `CancellationToken`, `AppEvent::ExportProgress`, never blocks frame; 1k-row windows | Full materialize Vec |
| D9 | **UI vs domain + error boundaries (§54, §33-34)** | Domain `AppState` (connections/schema/executions/results handles/tx) vs `UiState` (active_tab/splitter/expanded_nodes/scroll/focus/search) in `ui/layout`; render `fn show(..., &AppState, &mut UiState, &Sender<AppCommand>)` — no `async`/FS/SQL, deterministic; `AppError { op, pg_code, message, detail, hint, position, sanitized }` + per-pane `ErrorBanner`, never crashes app; `thiserror` domain, `anyhow` only at boundary | Mix `is_selected` into `Relation`; `anyhow` in public API |

**Dependency policy (§7, §45):** keep crate set as listed in `Cargo.toml` workspace deps; new additions justified per 8-question checklist: `arc-swap 1.7` (1 dep) for lock-free `SchemaModel` swap, `csv 1.3` + `serde_json 1.0` already in workspace, `sqlparser 0.51` optional feature, `tempfile` optional spill — all mature, `cargo-deny` audit.

---

## Recommended Approach

### Sequencing principle
* **Build bottom-up by dependency:** `value/model` (done) → `db/connection` → `db/execution/cancellation` → `results/stream/store/viewport` → `schema/introspection/cache/completion` → `storage/*` → `app` orchestration → `ui/*` → `export/edit/table_browser` → `observability/benches/docs`.
* **Keep render loop unlocked from day 1** — every I/O path must go through the `Command/Event` channel; add `#[deny(clippy::await_holding_lock)]`-adjacent manual review until `clippy` lint stabilizes.

### Step-by-step
1. **Scaffold `crates/db/*` (A1-A4):** create `Cargo.toml` + `src/lib.rs` for `connection`, `execution`, `cancellation`, `introspection`; add missing members to `Cargo.toml [workspace.members]`; define shared `types.rs` (`ConnectionId/QueryId/PgError/DbError/TxState/ConnectionState/SessionHealth`) in `connection` and re-export. Wire `tokio_postgres::Config` from `ConnectionConfig` + `MakeRustlsConnect`, spawn driver `Connection`, capture `CancelToken`, map `ReadyForQuery` → `TxState`.
2. **Execution + native cancellation (A5):** `execution/src/lib.rs` exposes `execute(query_session, sql) -> (QueryId, impl Stream<Row>)` + `cancel(CancelToken, host, port, ssl_mode)`. Map `tokio-postgres` error `57014` → `PgError{is_cancel}`. Add 3s cancel timeout → `SessionHealth::Poisoned`. Unit tests with mocked `CancelToken` + integration with `testcontainers` PG (long `pg_sleep` then cancel).
3. **Results pipeline (C1-C5):** implement `stream` (FromSql → `CellValue`), `store::BoundedStore` (ring buffer, configurable limit, `row_count` + `is_streaming`), `viewport::Viewport` (range calc + overscan). Add `results/value` already done — no change. Add property tests for backpressure (stream blocks when channel full).
4. **Schema introspection (B1-B4) + cache (B5-B7):** implement `introspection/queries.rs` 5 qualified queries + `hydrate.rs` via `schema::Builder`; fix `Builder::build` sort bug (sort by `(schema.name, name)`). Implement `schema/cache` with `arc-swap` + `CacheState` enum + `Event::SchemaReady`. Two-phase swap after Q3 vs Q4-6 as per B6.
5. **Storage + secrets (D3-D5):** implement `storage/{connections,history,preferences,editor_state}` with `rusqlite` migrations + `history` FTS5 + `keychain` + `secrecy`. Add `ConnectionConfig::sanitized_url()` for logs. Unit tests without PG, migration tests (v1→v2).
6. **Completion (B9-B10):** implement `schema/completion` pure crate (no tokio) with `CompletionEngine` + `extract_aliases` + 30 PG functions; ui/editor calls it synchronously. Add unit tests `SELECT u. FROM users u` → columns from `users`.
7. **App orchestration (D1-D2):** `crates/app` owns `ConnectionState` + `ExecutionState` + `SchemaCache`; command channel (`crossbeam bounded(64)`) + event channel; structured task ownership (`JoinHandle` + `CancellationToken`); lifecycle on window close → `DisconnectRequiresDecision` if `TxState != Idle`.
8. **UI layer (D6):** `crates/ui/{explorer,editor,results,connections,layout,theme,shortcuts}` — `explorer` renders `SchemaModel` tree (filterable), `editor` tabs + `CompletionEngine`, `results` virtualized grid via `Viewport` (measure frame time/allocation), `connections` form with `SslMode`/SSH, `layout` splitter + `theme` tokens. No SQL from render code.
9. **Table browser + editing + export (C3, C7-C8):** `table_browser` builds keyset pagination only when `Editability != Disabled`; `edit` builds parameterized `UPDATE … RETURNING *` with `GENERATED/IDENTITY` guards + conflict dialog; `export` streams CSV/JSON/SQL via `csv`/`serde_json` with correct quoting/NULL/bytea handling, backpressure-aware.
10. **Observability + benchmarks (D2, §4, §55, §66):** `crates/observability` captures startup/introspection/first-result/rows/bytes/cancel latency/store memory/frame time; `benches/` fixtures for 10k/100k/500k/1m rows + wide/large-text/JSONB; benchmark methodology recorded per §4.3 before any claim.

---

## Work Plan

| # | Track | Owner surface | Dependency | Work unit | Validation |
|---|---|---|---|---|---|
| **0** | All | `Cargo.toml`, `docs/decisions` | — | Add missing workspace members (`db/cancellation`, `db/introspection`, `schema/cache`, `schema/completion`, `results/stream`, `results/store`, `results/viewport`, `results/table_browser`, `results/edit`, `results/export`, `storage/*`, `ui/*`, `app`, `observability`); create `docs/decisions/ADR-0007-schema-cache.md` + `ADR-0008-connection-model.md` (Context/Decision/Alternatives/Tradeoffs/Consequences) | `cargo metadata --format-version 1` members == expected; `cargo test --workspace` compiles |
| **1** | A | `crates/db/connection` | 0 | Define `ConnectionId/QueryId/PgError/DbError/TxState/ConnectionState/SessionHealth`; `ManagedSession { client, driver_handle, cancel_token, health }`; `ConnectionConfig/ SslMode/SshTunnelConfig`; `connect(config) -> ManagedSession` via `tokio-postgres-rustls`; sanitize `Debug` + `tracing` | `cargo clippy -p pgnative-db-connection --all-features` green; unit test `Debug` redaction; `tracing` snapshot has no password |
| **2** | A | `crates/db/execution` + `cancellation` | 1 | `Execution { query_id, state, tx_state }` + `Command::Execute/Cancel` → `Event::ExecutionChanged/Rows`; `execution::execute(client, sql) -> Stream<Row>` via `query_raw` + `CellValue` decode; `cancellation::cancel(token, host, port, ssl_config)` on separate `TcpStream+rustls`; handle `57014`, races, `FailedRequiresReconnect` | `cargo test -p pgnative-db-execution` (mock cancel) + `testcontainers` PG 16: `SELECT pg_sleep(2)` → cancel → assert `Cancelled` + `SessionHealth::Ready` + rows stopped |
| **3** | C | `crates/results/stream` + `store` + `viewport` | 2 | `stream::ResultStream` (bounded 32, `Cancel` stops), `store::BoundedStore { capacity: 50000, Vec<Row> + eviction/optional spill, row_count, is_streaming }`, `viewport::Viewport::range(first, visible, overscan) -> Range<u64>`; large `Bytes` truncation | `cargo test -p pgnative-results-store` backpressure test: producer blocks when full; bench `viewport` 500k rows — frame <16ms with `overscan=20` |
| **4** | B | `crates/db/introspection` + `crates/schema/cache` + `model` fix | 1 | `introspection/{queries,hydrate}` 5 `pg_catalog` queries + `Builder` with `debug_assert` + fix `build.rs:81` sort to `(schema.name, name)`; `schema/cache::SchemaCache { ArcSwap<CacheState>, epoch, stale }` + two-phase swap + `Event::SchemaReady`; `SET TRANSACTION READ ONLY; statement_timeout=5s` | `cargo test -p pgnative-db-introspection` with `testcontainers` PG 15 against fixtures (§37: normal/composite PK/FK/views/functions/JSONB/arrays/no-PK/large); assert `SchemaModel::relation/editability/type_name` for `users` composite PK fixture; `cargo test -p pgnative-schema-model` after sort fix |
| **5** | D | `crates/storage/{connections,history,preferences,editor_state}` + `keychain` | 0 | `rusqlite` migrations (`user_version` 1→2) for `connections/history/preferences/editor_state`; `history` FTS5 `SELECT * FROM history_fts WHERE query MATCH ?`; `keychain` `set/get/delete` via `keyring` + `SecretString`; `sanitized_url()` | `cargo test -p pgnative-storage-*` (no PG): migration idempotence, FTS search, keychain fallback on `NoEntry`; `cargo clippy --workspace` |
| **6** | B | `crates/schema/completion` | 4 | `CompletionEngine::new(&SchemaModel)` indexes + `complete(CompletionContext { prefix, alias_map, dot_target }) -> Vec<CompletionItem>` capped 50 <1ms; `extract_aliases(sql, cursor)` regex + optional `sqlparser` feature; 30 common PG functions | `cargo test -p pgnative-schema-completion`: `SELECT u. FROM users u` → `users` columns, `SELECT * FROM public.users u WHERE u.` → alias; bench 10k relations prefix search p95 <1ms |
| **7** | D | `crates/app` | 1-5 | Own `ConnectionState` machine + `ExecutionState` map + `SchemaCache` + `HistoryStore` + result handles; `crossbeam bounded(64)` command + `bounded(32)` rows + event channel; `JoinHandle` lifecycle; on `TxState != Idle` window close → `Event::DisconnectRequiresDecision`; `observability` metrics hook | `cargo test -p pgnative-app` state-machine transition tests (Disconnected→Connected→Executing→Cancelling→Connected/Poisoned); close-with-Tx test asserts decision required |
| **8** | D | `crates/ui/{explorer,editor,results,connections,layout,theme,shortcuts}` | 6,7,3 | Explorer tree (filterable by schema/relation/function, FK in/out), Editor tabs (tabs, syntax hl, `execute statement/selection` Cmd+Enter, `cancel` Esc, `CompletionEngine` on every keystroke with zero I/O), Results virtualized grid via `Viewport` (visible+overscan, stable IDs, hex for `Bytea`, truncate large `Bytes`), Connections form (SslMode radio, SSH tunnel isolated per §25), Layout splitter + Theme tokens, Shortcuts documented (§32) | `cargo test -p pgnative-ui-*` command-routing + tab lifecycle; manual key binding check vs platform conventions; `cargo xtask bench-ui` if available or `benches/` scroll FPS >55 at 500k |
| **9** | C | `crates/results/table_browser` + `edit` | 4,7,3 | `table_browser::browse(relation, limit, keyset)` builds `SELECT … FROM … ORDER BY pk LIMIT $1` or `WHERE pk > $1` only when `Editability`; `edit::identify/diff/update RETURNING *` with `GENERATED/IDENTITY/Virtual` guards + `DiffView` confirm + optimistic concurrency (`WHERE pk AND …`) | `testcontainers` PG: edit `users` PK row → assert parameterized `UPDATE` + `RETURNING *` reflected in store; edit composite-PK `order_items`; edit attempt on `no-pk` table → `Disabled`; concurrent-mod conflict dialog |
| **10** | C | `crates/results/export` | 3,7 | `export::{Csv,Json,SqlInsert}` streaming writers: CSV `csv` quoting (commas/quotes/newlines/NULL→empty, UTF-8), JSON typed (`Null→null`, `Numeric→string|number`, `Bytea→base64`, `Uuid/Timestamp→RFC3339`), SQL INSERT `postgres-protocol` escaping + parameterized statement template; backpressure via `Stream` | `cargo test -p pgnative-results-export` fixtures: commas/quotes/newlines/NULL/bytea/jsonb/timestamp round-trip; export 1m rows → memory bounded (§28 — never full materialize), bench bytes/sec |
| **11** | D | `crates/observability` + `benches` + `docs` | 7 | `observability::{startup, introspection, first_result, rows, bytes, cancel_latency, store_memory, frame_time}` via `tracing` spans (debug-only); `benches/` fixtures (10k/100k/500k/1m, wide, large-text, JSONB, mixed types §36.4); `docs/benchmarks/` per §4.3 (hw/OS/PG/query/network/build mode/caches); troubleshooting doc | `cargo bench --workspace` reproduces §4.1 targets on reference machine; `docs/benchmarks/README.md` contains methodology + numbers, not fabrications |

**PR slicing note (Binding per Executing The Plan):** if asked to publish, publish exactly the work units above as separate commits/PRs in dependency order (0→11), never collapsed. Unit 0 must be its own commit (manifest + ADRs).

---

## Validation Plan

### Focused commands (run narrow → broad, per §59)
```bash
# 1. Formatting & lints (gate per clippy.toml)
cargo fmt --check
cargo clippy --workspace --all-targets --all-features

# 2. Unit tests (no PG)
cargo test --workspace -- --test-threads=4

# 3. Schema model specifically
cargo test -p pgnative-schema-model -- --nocapture

# 4. Per-crate after each unit
cargo test -p pgnative-db-connection
cargo test -p pgnative-db-execution
cargo test -p pgnative-results-store
cargo test -p pgnative-schema-completion
cargo test -p pgnative-storage-history

# 5. Integration (requires Docker; real PG per §36.2, never mocked for PG semantics)
cargo test --workspace --test integration -- --ignored  # or cargo test -p pgnative-db-introspection -- --ignored
# fixtures (§37): normal, composite PK, FKs, views, functions, nullable, generated/identity, JSONB, arrays, timestamps, UUIDs, large text, no-PK, large datasets + awkward schemas

# 6. Performance (when benches land)
cargo bench -p pgnative-results-store
cargo bench -p pgnative-schema-completion
```

### E2E / black-box checks (cannot be automated in unit)
* **Connect E2E:** launch app → save connection `postgres://user:****@localhost/test` → Connect → tree populated progressively (Phase A <500ms, Phase B banner disappears) → `SELECT 1` in editor → first rows <100ms local PG → scroll 100k rows no jank.
* **Cancellation E2E:** `SELECT pg_sleep(5)` → Cancel within 500ms → backend `pg_stat_activity` no longer shows query → UI shows `Cancelled (57014)` → new `SELECT 1` succeeds without reconnect → if inside `BEGIN; SELECT pg_sleep(5);`, cancel leaves `InFailedTransaction` badge → `ROLLBACK` clears badge.
* **Editing E2E:** browse `users` (PK table) → double-click cell → change `email` → DiffView shows before/after → Confirm → `UPDATE users SET email=$1 WHERE id=$2 RETURNING *` observed in PG logs (parameterized, not concat) → store reflects returned row; try editing `logs_no_pk` → inline editing disabled; `GENERATED` column not editable.
* **Export E2E:** run `SELECT * FROM large_table` (10k rows with commas/quotes/newlines/NULL/bytea) → Export CSV → open in spreadsheet validator: quoted correctly; Export JSON → `jq .` valid, `null` not `"NULL"`; Export SQL → psql replay inserts correctly.
* **Secrets E2E:** `tracing` output for `ConnectionConfig { Debug }` shows `password: ***`; SQLite `connections` table contains no raw password column; keychain entry deletable independently.

### Highest-risk validation
**Cancellation + Tx state + large-result backpressure.** If cancellation leaves protocol desync (`Poisoned`) and next query silently reuses the session, data corruption or phantom `InTransaction` follows (§11, §53). Validate via `testcontainers` race: fire `SELECT pg_sleep(2)` + `Cancel` at 100ms, 500ms, 1500ms (before/during/after rows), after `BEGIN`, with network drop — assert every transition ends in `Ready` or explicit `Poisoned` requiring reconnect, never silent `Ready` with wrong `TxState`.

---

## Risks / Rollback

| Risk | Impact | Mitigation / Rollback |
|---|---|---|
| `tokio-postgres` `CancelToken` API changes or `CancelRequest` blocked by network/firewall | Cancellation degrades to future-drop | Abstract `cancellation::Cancel` trait; integration test probes real `57014`; fallback surfaces `CancellationState::FailedRequiresReconnect` and forces reconnect, never silent failure |
| Introspection `pg_catalog` queries drift across PG 15→17 (`attgenerated/attidentity` added in 12) | Model hydration panic / missing `ValueSource` | Version-gated queries (`SHOW server_version_num`), feature-detect `attidentity` column existence via `SELECT to_regclass`; fallback to `Stored` when unknown; `testcontainers` matrix PG 15/16/17 |
| `Builder::build` sort bug fix changes deterministic order → snapshot test churn | Tree diff | Document sort contract `(schema.name, relation.name)` in `ADR-0007`; update snapshots once; add `sorted_relations` invariant test |
| `arc-swap` addition violates §7 dependency cost or adds to binary | Small binary/startup cost | Evaluate `parking_lot::RwLock<Arc>` alternative (in-tree, zero new dep) as fallback; lock held only during swap (ns), not read |
| Virtualized rendering still janks at 500k (egui `Table` overhead, large `Bytes` formatting per frame) | UX regression §18 | Truncate `Bytes` >1k with expand button, cache formatted strings keyed by `Row.index`, small overscan (20), avoid per-frame `to_string` — measure `frame_time` via `observability` and revert `Viewport` overscan if >16ms |
| `keyring` platform parity (macOS keychain vs Win vs `secret-service` D-Bus) | Secrets unavailable on Linux CI | Feature-gate `keyring` per ` Cargo.toml` already (`apple-native`, `windows-native`, `sync-secret-service`, `crypto-rust`); test fallback: if keychain fails, keep `SecretString` in-memory for session with warning (never SQLite fallback) |
| SQLite `FTS5` history query perf on huge history | Search slow | Cap FTS index to last 10k entries (configurable) + pagination; history export/truncate affordance |
| Export streaming backpressure stalls result stream | Export hangs query | Export copies from `BoundedStore` snapshot, not live stream; live export via `Stream::clone` with separate cursor |

**Rollback general:** each crate is feature-gated via `Cargo.toml` members — remove member from workspace to exclude from build; migrations are `user_version` versioned, never in-place destructive ALTER without `IF EXISTS`.

---

## Open Questions

1. **System schemas visibility:** include `pg_catalog` + `information_schema` in `SchemaModel` always but hide in UI via setting? **Assumption for v1:** yes — model includes them, `explorer` filters `pg_catalog/pg_toast/pg_temp%` by default with `[ ] Show system schemas` toggle. Confirm product owner does not want them excluded from model (affects `completion` when user types `pg_catalog.`)
2. **Completion parser dependency:** `sqlparser 0.51` optional feature vs hand-rolled only. **Assumption:** hand-rolled `FROM/JOIN` regex + quoted-identifier splitter is sufficient for `alias.` cases per §14, full `sqlparser` deferred to `alias + subquery` bug reports. Open until measured miss rate >20%.
3. **Transaction status source:** authoritative `ReadyForQuery 'I'/'T'/'E'` via `tokio-postgres` hook not publicly exposed — may need `postgres-protocol` patch or piggyback `SHOW` plus classifier. **Gap:** research `tokio_postgres::Connection::parameter` vs `ready_for_query` exposure before unit 2; if unavailable, document optimistic+poll-on-next-query as v1.
4. **Spill-to-disk threshold for `BoundedStore`:** when is `tempfile` spill vs pure in-memory eviction (lossy) vs explicit pagination? **Assumption:** v1 bounded in-memory 50k-row window evicts oldest (lossy but scrollable within window); spill-to-disk deferred until large-result scrolling beyond window is user-reported.
5. **Benches reference hardware:** §4.3 requires hw/OS/PG/query/network/mode — which reference machine defines `<300ms cold start / <80MB` budgets for CI gate? **Open:** owner to name reference (e.g., `M2 16GB / Ubuntu 24.04 / PG 16 / local / release / cold`).

None block scaffolding (unit 0-1); resolve 1-3 before unit 4-6, 4-5 before unit 3 final tuning.

---

## Sources

* Workspace facts: `/home/sachin/Desktop/Code/pgnative/Cargo.toml`, `/home/sachin/Desktop/Code/pgnative/crates/schema/model/src/types.rs:11`, `relation.rs:32`, `column.rs:8`, `schema.rs:7`, `index.rs:18`, `build.rs:13`, `crates/results/value/src/lib.rs:22`, `rust-toolchain.toml`, `clippy.toml`
* Product contract: `/home/sachin/Desktop/Code/pgnative/AGENTS.md` (§§1-5, 8-14, 15-22, 24, 27-30, 36, 41, 53-55)
* Planners: subagent `01a05754-8eb1` (DB) + `01a05754-9dd5` (Schema) + `01a05754-ad0c` (Results) + `01a05754-bdb5` (App/Storage/UI) — all extracted from `subagent/*/session.jsonl`; Results/App arrived late, merged without overwriting audited DB/Schema
* External libs only via workspace deps already pinned — `tokio-postgres 0.7` `CancelToken::cancel_query`, `arc-swap 1.7`/`csv`/`sqlparser`/`tempfile` CRDs (mature, audited — use official docs.rs when introducing); no discovery-only sources used as final evidence

