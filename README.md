# pgNative

A fast, native PostgreSQL client for developers who just want to query, inspect, and edit their database.

PostgreSQL only. No Electron. Local desktop app (Rust + egui). The UI never blocks on database I/O: connections, queries, schema introspection, and exports all run async with real PostgreSQL cancellation.

## Install

Prerequisites: Rust stable (see `rust-version` in `Cargo.toml`) and a C toolchain for the bundled SQLite.

```sh
git clone https://github.com/sandy-sachin7/pgnative
cd pgnative
cargo run -p pgnative-app --release
```

Or install the binary into `~/.cargo/bin`:

```sh
cargo install --path crates/app
```

This is a developer build. Signed installers (`.dmg` / `.msi` / AppImage) do not exist yet.

## First connection (30 seconds)

1. Open the **Connections** panel at the bottom.
2. Paste a connection URL (recommended), e.g. `postgres://user:pass@localhost:5432/mydb?sslmode=prefer`, and fill **Password** if it isn't in the URL.
   - Or fill the manual fields: Name / Host / Port / Database / Username + SSL mode (`disable`, `prefer`, `require`, `verify-ca`, `verify-full`).
3. Press **Connect**.

Non-secret settings are stored in local SQLite; the password goes to the OS keychain, never to disk in plaintext. Connection errors show inline under the form.

The starter tab already contains `SELECT 1;` — press **Ctrl+Enter** to run it.

## Querying

- **Run**: `Run` button or `Ctrl+Enter` runs the active tab on the active connection.
- **Cancel**: `Cancel` button or `Esc` sends a real PostgreSQL `CancelRequest` (not just dropping the future). Safe against 500k-row sequential scans.
- **Refresh schema**: `F5` or the `Refresh Schema` button re-introspects after DDL.
- **Multiple tabs**: `New Tab` in the top bar; click a tab name to switch. Each run clears the previous result — queries never mix.
- **Completion**: typing `users.` or an alias like `u.` (after `FROM users u`) opens a popup with columns (up to 8 shown). Click an entry to insert it. Alias-aware; schema-qualified prefixes work too.
- **Results**: virtualized grid — only visible rows render, so 100k+ row results scroll smoothly. The store is bounded (50k rows / 64MB); oldest rows evict with a `…` placeholder. Cells truncate at 2KiB with a byte count.
- **History**: right panel. Type to FTS-search past queries (empty query shows the 20 most recent). Click any entry to load it into the editor and re-run.
- **Export**: `CSV` button saves the active (or last finished) query's buffered rows to `$TMPDIR/pgnative-export-<query-id>.csv`. CSV only for now.

## Transactions

The top bar shows a green **TX** badge (red **TX ERR** if the transaction failed) whenever a session is inside a transaction.

Pressing **Disconnect** with an open transaction opens a choice instead of silently committing or rolling back:

- **Commit + disconnect**
- **Rollback + disconnect**
- **Keep open** (stay connected, decide later)

## Editing safety

Inline row editing is **not yet exposed in the UI** — the results grid is read-only. The safety model already exists at the crate level (`pgnative-results` edit module) and the UI will only enable editing once it can prove a safe row identity:

- allowed only with a primary/unique key (composite keys supported);
- parameterized `UPDATE ... WHERE pk ... RETURNING *`, never string concatenation;
- stale-write guard via optimistic `AND col = old` checks;
- blocked inside explicit transactions and for `MERGE`.

## Shortcuts

| Keys | Action |
|---|---|
| `Ctrl+Enter` | Run active tab |
| `Esc` | Cancel running query |
| `F5` | Refresh schema |

## Troubleshooting

- **"unknown connection" / connect fails**: the form builds the config at click time — check host/port/dbname and that the server accepts TCP. With `sslmode=verify-full`, the hostname must match the server certificate.
- **Blank results after connect**: run a query first — the grid shows the last executed result, cleared on every run.
- **History panel says "No history yet"**: run any query; history records on completion.
- **Export says "only CSV export is supported"**: JSON / SQL-INSERT buttons were removed until they're real; CSV covers the need.
- **Completions don't appear**: they need an introspected schema (connect first) and a non-empty prefix or `alias.` target; the popup is click-to-insert (no keyboard accept yet).

## Architecture (short)

```text
egui UI (app::ui) → commands → Tokio runtime → PostgreSQL
                        ↑ events (bounded channels, UI never blocks)
```

Crates: `app` (UI + runtime), `db` (connection, introspection), `results` (value, stream, store, viewport, portal, table_browser, edit, export), `schema` (model, cache, completion), `storage` (connections, history, editor_state, preferences, keychain).

Product rules live in `AGENTS.md`; design decisions in `docs/decisions/`.

## Tests

```sh
cargo test --workspace   # unit + integration (integration needs Docker for PG16 testcontainers)
cargo fmt --check
cargo clippy --workspace --all-targets --all-features
```

See `docs/benchmarks.md` for the benchmarking methodology (no published numbers yet).
