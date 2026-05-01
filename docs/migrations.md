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

- **How it works:** `caqui` determines the required structural changes based on your `schema.cq` and your existing migration history. It generates a new timestamped SQL file (e.g., `20231027120000_auto_migration.sql`) in your `migrations/` directory and applies it.
- **Best for:** Collaborative development, staging, and production environments.
- **Note:** This maintains an auditable history of all database structural changes.

## Safety and Rollbacks

`caqui` prioritizes data safety while allowing for structural evolution. All structural changes are applied safely within a transaction. If an error occurs during the process (such as a constraint violation), the changes are automatically rolled back, leaving your database in its previous consistent state.