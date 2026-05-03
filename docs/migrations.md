# Database Lifecycle & Migrations

`caqui` provides two primary workflows for evolving your database schema: a fast, iterative workflow for local prototyping and a safe, historical workflow for production-ready deployments.

## The Prototyping Workflow (`push`)

When you are rapidly iterating on your schema and don't care about maintaining a history of individual SQL migration scripts, use `push`.

```bash
caqui schema push
```

- **How it works:** `caqui` calculates the structural difference between your current database and your `schema.cq`, applying the changes immediately.
- **Best for:** Local development and rapid prototyping.
- **Note:** This command modifies the database directly without generating any migration history files.

## The Migration Workflow (`migrate`)

For shared environments and production deployments, use `migrate`. This workflow ensures that every schema change is captured as a versioned SQL script.

```bash
caqui schema migrate
```

## Expert: The Shadow Database Architecture

When you run `caqui schema migrate`, the engine does not simply compare your schema to your live database. Instead, it guarantees determinism using a "Shadow Database":

1. **In-Memory Shadow**: It spins up an empty, in-memory SQLite database.
2. **Replay History**: It applies all existing `.sql` migration files from your `/migrations` directory sequentially to the shadow database.
3. **Introspection**: It introspects the resulting "historical" database state.
4. **Diff Generation**: It compares this historical state against your current `schema.cq` AST.
5. **Script Generation**: If there are differences, it generates a new timestamped SQL file (e.g., `1698408000_auto_migration.sql`) and applies it to the live database.

This guarantees that your migration history files are always the absolute source of truth for the database structure.

## Expert: Schema Diffing & Operations

The `caqui` differ intelligently calculates the most efficient SQL operations required to transition the database state.

- **Fast Alterations**: Operations like adding a new column or creating an index are executed as fast `ALTER TABLE ADD COLUMN` or `CREATE INDEX` commands.
- **Additive-Only Columns**: By design, dropping a column from `schema.cq` **does not** automatically drop it from the SQLite database. This protects against accidental data loss. The column is simply ignored by the API. It will only be physically dropped if the table is eventually rebuilt for another reason (like a type change).
- **Complex Mutations**: SQLite has severe limitations on `ALTER TABLE` (e.g., you cannot drop columns, change column types, or alter constraints). When `caqui` detects these changes, it triggers a **Table Rebuild**.

### The SQLite Rebuild Process (Technical Deep Dive)

When a table must be rebuilt to bypass SQLite's limitations, `caqui` executes a safe, multi-step process wrapped in a transaction:

1. `PRAGMA foreign_keys=OFF;` (Disables FK enforcement temporarily).
2. `BEGIN TRANSACTION;`
3. Creates a temporary table (`_engine_new_TableName`) with the new desired structure.
4. Uses `INSERT INTO ... SELECT ...` to copy all data for shared columns from the old table to the temporary table.
5. `DROP TABLE TableName;`
6. `ALTER TABLE _engine_new_TableName RENAME TO TableName;`
7. Recreates all associated indexes and triggers.
8. `PRAGMA foreign_key_check;` (Verifies no orphan records were created).
9. `COMMIT;`
10. `PRAGMA foreign_keys=ON;`

This ensures that data is preserved safely during complex structural schema changes.

## Expert: Full-Text Search (FTS) Migrations

When you use the `@@fulltext` attribute, `caqui` automatically provisions and manages SQLite FTS5 virtual tables. 
- During a table creation or rebuild, it automatically emits the `CREATE VIRTUAL TABLE ... USING fts5` statements.
- It automatically manages the synchronization triggers (`INSERT`, `UPDATE`, `DELETE`) that keep the FTS shadow table in sync with the primary table.

## Tracking Migrations

To track migration telemetry, `caqui` maintains a hidden internal table: `_engine_migrations`. This table acts as a simple execution counter, inserting a new row with a timestamp every time the auto-migration process is completed against the live database. It does not enforce idempotency; the engine relies on the intelligent differ to avoid applying the same changes twice.
