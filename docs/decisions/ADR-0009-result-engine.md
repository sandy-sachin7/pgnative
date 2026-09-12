# ADR 0009 — Result Engine

**Status:** Accepted — 2026-09-01  
**Context:** §15, §16, §17, §18, §19

## Context

pgNative must remain responsive for 500k+ rows (§4.1) without equating `result set = Vec<Row>` (§15). Two workloads share the engine (§17): arbitrary SQL (unordered, projected, joined) and table browsing (ordered by PK). Prior ADRs fix connection (§9, ADR-0008) and schema cache; result rendering must be virtualized (§18) and type-faithful (§19) while never silently rewriting user SQL with `LIMIT`/`OFFSET` (§16).

## Decision

Pipeline `PostgreSQL → Result Stream → Bounded Result Store → Viewport → Virtualized egui grid` (`crates/results/{stream,store,viewport}`) with explicit window fetch via server-side portal (DECLARE CURSOR / FETCH), not OFFSET:

- **Stream** (`stream` crate): `tokio_postgres::query_raw` → `StreamEvent::Meta/Batch/Complete` with bounded `mpsc::channel(cap=16)`, `batch_size=64`, `per_cell_cap=64 KiB`, backpressure via `send().await`. `decode_cell` maps OIDs to `CellValue` (bool/int/numeric/date/time/timestamp/timestamptz/uuid/json/bytea/array/text) and truncates large cells at UTF-8 boundary.
- **Store** (`store` crate): ring eviction with `row_budget=50k` + `byte_budget=64 MiB`, stable `Row.index`, `snapshot_range(offset,len) -> Arc<[Row]>` under `RwLock` (no await inside lock, sole Tokio writer).
- **Viewport** (`viewport` crate): `ViewportState::visible_range` + `fetch_range` (double overscan, f64 height math) and `snapshot(&store) -> ViewportSnapshot { rows: Arc<[Row]>, total }`; `show_rows`-compatible virtualization — only visible + overscan rows create widgets.
- **Table browser** (`table_browser`): keyset `WHERE (pk) > ($X) ORDER BY pk LIMIT $n`, never OFFSET, only when `Editability != Disabled`.
- **Portal / window fetch** (`portal` crate): for arbitrary SQL that must not materialize fully, use a *transaction-bound* portal:

  ```sql
  BEGIN READ ONLY;
  DECLARE "pgnative_portal_<uuid>" CURSOR FOR <user_sql_unmodified>;
  FETCH FORWARD 500 FROM "pgnative_portal_<uuid>";  -- repeated window fetches
  CLOSE "pgnative_portal_<uuid>";
  COMMIT;  -- or ROLLBACK on error/cancel
  ```

  No `LIMIT`/`OFFSET` is injected into `<user_sql_unmodified>`. Portal lives on a dedicated session (separate from foreground `query_session`) so `BEGIN` does not pollute the user's transaction state. Each `FETCH FORWARD n` decodes via same `decode_cell_with_cap` path and appends to `ResultStore` window. Cancellation = `CancelToken::cancel_query` + `CLOSE` + `ROLLBACK`, leaving session `Ready` (authoritative `ReadyForQuery I`).

## Alternatives

- **Materialize all rows**: unbounded memory, freezes UI (§15 rejected).
- **Rewrite with OFFSET/LIMIT**: changes semantics (non-deterministic order, cost O(offset)), rejected by §16.
- **Client-side OFFSET over cached full result**: still requires full network transfer.
- **Keyset for arbitrary SQL**: requires PK/order assumption that arbitrary projections lack (§17).
- **Scrollable cursor `FETCH ABSOLUTE`**: random access is not free over SQL cursors; window FETCH FORWARD + optional `MOVE` suffices for viewport scrolling without implying cheap random access.

## Consequences

- `egui` thread never blocks on PG; `AppRuntime` owns fetch tasks, UI pulls `snapshot`.
- Large results stream progressively; eviction keeps memory bounded and is exposed to UI via `is_truncated(total_pushed)`.
- Portal API surfaces `declare_portal / fetch_forward / close_portal` with `PortalError` preserving `PgError`/`sqlstate`.
- Cursor requires a transaction — portal module documents `declare` → `BEGIN`, `close` → `COMMIT`/`CLOSE`/`ROLLBACK` and is incompatible with explicit user transactions on the same session (callers must use the meta/portal session).
- Binary vs text decoding already proven in `stream` matrix tests; portal reuses it.

## Tradeoffs

- Hold open transaction for portal lifetime (server holds cursor + snapshot). Mitigated by dedicated session, short windows, and explicit `CLOSE`; long-idle portals are closed on tab close.
- Two fetch paths (stream vs portal) — justified because arbitrary SQL cannot use keyset pagination without rewriting semantics.
- Extra round-trips per window vs one streaming pipeline; wins when result >> budget or when user never scrolls to tail.
