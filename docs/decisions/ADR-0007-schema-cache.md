# ADR 0007 — Schema Cache Strategy

**Status:** Accepted — 2026-08-31  
**Context:** §12, §13, §14 — Schema introspection powers tree, autocomplete, editing safety.

## Context
Need a single canonical `SchemaModel` that is available off the DB thread, refreshed explicitly, and never queried per keystroke.

## Decision
- In-memory `Arc<SchemaModel>` behind `parking_lot::RwLock<Arc<SchemaModel>>` (defer `arc-swap` until measured contention >1% frame time).
- Two-phase introspection: Phase A (schemas+relations+columns+types) → first swap → UI renders; Phase B (PKs/unique/FKs/functions/comments) → second swap.
- Dedicated read-only introspection session (`statement_timeout=5s`, `SET TRANSACTION READ ONLY`, qualified `pg_catalog`).
- Cache state: `Empty | Loading{since} | Ready{model:Arc<SchemaModel>, epoch:u64, stale:bool} | Error{msg}`.
- Refresh: explicit command + heuristic `command_tag ~ CREATE|ALTER|DROP` → mark stale + banner.
- No SQLite persistence v1.

## Alternatives
- SQLite-mirror: doubles migration risk, stale bugs.
- All-or-nothing: blocks explorer on `pg_proc`.
- Per-schema lazy: N+1, violates §12.

## Consequences
- `SchemaCache::get()->Arc<SchemaModel>` is lock-free for readers (clone Arc under read lock).
- `CompletionEngine` rebuilt on swap.
- Requires dedicated session per §9.

## Tradeoffs
- `arc-swap` deferred: `RwLock<Arc>` read lock held only for `Arc::clone()` (~ns), acceptable until contention measured.
