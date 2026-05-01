# CLI Commands Reference

The `caqui` engine is packaged as a single executable binary that provides several commands to manage your database lifecycle, run the API server, and interact with version control.

## Project Initialization

### `caqui init`
Generates a starter `schema.cq` file in the current directory if one does not already exist. This file is the declarative source of truth for your database schema and API structure.

## Schema Management

### `caqui schema db-push`
Quickly syncs your `schema.cq` to your local SQLite database (`app.db`). This instantly generates and applies structural differences to the database. This command is best used during local prototyping and development.

### `caqui schema migrate-dev`
The safe, historical deployment workflow. This command:
1. Reads your `schema.cq`.
2. Compares it against your previous migration files in the `migrations/` directory.
3. Generates a new `_auto_migration.sql` script.
4. Safely applies the new script to your live database.

## API Server

### `caqui api start`
Starts the database connection pool and mounts the universal dynamic execution router.
- By default, it binds to `http://0.0.0.0:4000`.
- You can override the port by setting the `PORT` environment variable.

## Git Proxy & Specialized Behaviors

Because `caqui` utilizes a decentralized concurrency model based on Git, it provides a proxy command (`caqui git`) that intercepts and modifies certain Git operations to ensure database integrity across distributed instances.

### `caqui git <args>`
Passes commands directly to the underlying `git` executable, but intercepts specific subcommands to enforce the custom `sqlitevfs` merge driver.

#### Specialized `merge` Behavior
When you run `caqui git merge <branch>` (or related commands like `pull`), the CLI automatically injects the `-s sqlitevfs` strategy flag.
- **Enforced Strategy:** The custom `sqlitevfs` driver is strictly enforced to safely reconcile SQLite binary states globally.
- **Overrides Prevented:** If you attempt to pass a custom strategy (e.g., `-s recursive` or `--strategy=...`), the CLI will block the command and return an error to prevent database corruption.

#### Specialized `diff` Behavior
When you run `caqui git diff`, the CLI intercepts the command and runs a custom internal diffing engine tailored for comparing SQLite schemas and states, rather than showing a binary diff of the `app.db` file.

#### Automatic Driver Setup
Running `caqui git` commands will automatically bootstrap the custom Git VFS and merge driver into a hidden `.caqui/bin/` directory and temporarily add it to your `PATH` for the duration of the command. This ensures Git can always discover the `git-merge-sqlitevfs` binary without requiring a manual, system-wide installation.
