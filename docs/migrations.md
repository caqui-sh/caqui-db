# Database Lifecycle & Migrations

`caqui` provides two primary workflows for evolving your database schema: a fast, iterative workflow for local prototyping and a safe, historical workflow for production-ready deployments.

## The Prototyping Workflow (`db-push`)

When you are rapidly iterating on your schema and don't care about maintaining a history of individual SQL migration scripts, use `db-push`.

```bash
caqui schema db-push
```

- **How it works:** `caqui` introspects your current `app.db`, calculates the structural difference with your `schema.cq`, and applies the changes immediately.
- **Best for:** Local development and initial prototyping.
- **Warning:** This command does not generate a `.sql` file in your `migrations/` directory.

## The Migration Workflow (`migrate-dev`)

For shared environments and production deployments, use `migrate-dev`. This workflow ensures that every schema change is captured as a versioned SQL script.

```bash
caqui schema migrate-dev
```

### How `migrate-dev` Works

1. **Shadow Database:** `caqui` spins up a temporary, in-memory "Shadow Database".
2. **Replay:** It replays all existing scripts in your `migrations/` folder into the Shadow Database to reach the current "production" state.
3. **Diff:** It compares the Shadow Database schema against your desired `schema.cq`.
4. **Generate:** If there is a difference, it generates a new timestamped SQL file (e.g., `20231027120000_auto_migration.sql`) in the `migrations/` folder.
5. **Apply:** Finally, it applies the new script to your live `app.db`.

## Handling Destructive Changes

`caqui` prioritizes data safety but allows for structural evolution.

### Table Rebuilds
Some changes in SQLite (like changing a column type or adding a `UNIQUE` constraint to an existing column) require rebuilding the entire table. `caqui` handles this automatically:
1. Creates a temporary table (`_engine_new_TableName`).
2. Migrates all existing data to the new table.
3. Drops the old table and renames the new one.

### Data Coercion
SQLite uses "Manifest Typing," meaning it is flexible with data types. If you change a column from `String` to `Int`, `caqui` will perform the table rebuild. Existing string values like `"42"` will be converted to integers, while non-numeric strings like `"hello"` will be stored as strings in the integer column (per SQLite's dynamic nature) unless you are using `STRICT` tables.

### Migration Safety
If a migration fails (e.g., due to a constraint violation like a `NULL` value in a new `NOT NULL` column), `caqui` will roll back the transaction, leaving your database in its previous consistent state.
