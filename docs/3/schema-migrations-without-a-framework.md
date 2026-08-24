---
id: schema-migrations-without-a-framework
title: 'Schema migrations: idempotent ALTER TABLE, checked via pragma'
altitude: 3
topics:
- storage
relations:
- type: part_of
  target: local-message-store
summary: Adding a column to the messages or folders table needs both a SCHEMA entry and an idempotent ALTER TABLE in Store::migrate, checked via PRAGMA table_info.
---

# Schema migrations: idempotent ALTER TABLE, checked via pragma

There is no migration framework, no version table, and no `user_version` pragma
in `crates/birdman-store/src/lib.rs`. Adding a column to the `messages`, `folders`,
or any other table has a specific two-part procedure, and skipping half of it
silently breaks every existing install while working perfectly on your machine.

## Why `CREATE TABLE IF NOT EXISTS` isn't enough

The `SCHEMA` constant's `CREATE TABLE IF NOT EXISTS` statements only help a
**brand-new** database. A column added to `SCHEMA` after a database already
exists on someone's disk is simply never created there — the `CREATE` is skipped
wholesale, columns and all.

So every such column needs an explicit, idempotent `ALTER TABLE` in
`Store::migrate` **in addition to** being in `SCHEMA`. Both, not either.

## The pattern

```rust
let has_from_name: bool = conn
    .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'from_name'")?
    .exists([])?;
if !has_from_name {
    conn.execute("ALTER TABLE messages ADD COLUMN from_name TEXT", [])?;
}
```

Two existing entries follow it: `messages.from_name` and `folders.special_use`.

The `PRAGMA table_info` check is deliberate rather than just running the
`ALTER TABLE` and ignoring a "duplicate column" error — swallowing errors there
would also swallow a real failure, like a locked or corrupt database, alongside
the expected "already migrated" case.

## Consequences

- New columns must be nullable or have a default; `ALTER TABLE ADD COLUMN` can't
  add a `NOT NULL` column without one to an existing table.
- Backfilling existing rows isn't automatic. `folders.special_use` is the worked
  example: `AppState::sync_now` re-lists folders partly so an existing local
  database whose `special_use` hadn't been backfilled picks it up without
  restarting the app.
- `migrate` runs on every open, inside `Store::init`, after the `SCHEMA` batch.
- Renaming or dropping a column is not supported by this pattern at all.
