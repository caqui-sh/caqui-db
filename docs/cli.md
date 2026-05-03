# CLI Commands Reference

`caqui` is packaged as a unified, zero-dependency executable. It provides a cohesive toolset to manage your database lifecycle, run the dynamic API server, and safely interact with version control in a decentralized environment.

## Project Initialization

### `caqui init`
Generates a starter `schema.cq` file in the current directory if one does not already exist. This file acts as the declarative source of truth for both your database structure and API schema.

## Schema Management

`caqui` provides two primary strategies for synchronizing your database state with your `schema.cq`.

### `caqui schema push`
The rapid-prototyping workflow. 

This command parses the AST, calculates the structural differences, and applies them **directly** to `app.db`. 
- **Use Case**: High-velocity local iteration.
- **Limitation**: It does not maintain a migration history.

### `caqui schema migrate`
The safe, production-grade deployment workflow. 

This command:
1. Spawns a temporary "shadow" database.
2. Introspects the differences between the current schema and the shadow state.
3. Generates a new, timestamped `.sql` migration script in the `migrations/` directory.
4. Safely applies the new script to your live database.
- **Use Case**: Safe evolution and CI/CD pipelines.

## API Server

### `caqui api start`
Starts the database connection pool using the Custom SQLite VFS and mounts the universal dynamic execution router.

- **Prerequisite**: The `app.db` file must exist (created via `schema push` or `schema migrate`) before the server can start.
- **Binding**: Binds to `0.0.0.0` by default.
- **Port**: Listens on port `4000`. You can override this using the `PORT` environment variable.

```bash
PORT=8080 caqui api start
```

## Git Proxy & Specialized Behaviors (`caqui git`)

Because `caqui` utilizes a decentralized concurrency model based on Git, it provides a specialized proxy command: `caqui git`. 

This proxy passes arguments directly to the underlying `git` executable but intercepts specific subcommands to ensure database integrity across distributed instances. Internally, the CLI automatically configures and injects the `git-merge-sqlitevfs` driver into your `PATH`.

### Specialized `merge` Behavior
When you run `caqui git merge`, the CLI intercepts the command and automatically enforces the custom `-s sqlitevfs` merge strategy. 

**Expert Note**: Attempts to manually override this strategy (e.g., `caqui git merge -s recursive`) will be actively rejected by the CLI with an error. This strict enforcement prevents binary corruption of the `app.db` file.

### Specialized `diff` Behavior
When you run `caqui git diff [old_ref]`, the CLI completely bypasses the standard Git diff output. 

Instead, it invokes a custom internal diffing engine that compares the raw `schema.cq` files and outputs a human-readable structural diff of the schema changes and potential state conflicts, rather than useless binary blob diffs.
