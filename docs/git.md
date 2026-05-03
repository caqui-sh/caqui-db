# Decentralized Git Workflow

`caqui` introduces a paradigm shift in database management: a decentralized concurrency model. Instead of relying on a single, centralized database server, **each user, client, or environment operates on their own unique local copy of the repository and the database.**

In this model, your `app.db` and `schema.cq` are versioned together. Git acts as the ultimate source of truth, synchronizing both the structural schema and the underlying data.

## Branching Strategies

Because your database state is tied to your Git tree, branching in `caqui` isolates not just your code, but your data.

- **Feature Isolation**: When you create a new Git branch for a feature, you also create an isolated database branch. You can apply schema migrations and run data mutations locally without affecting the `main` branch.
- **Safe Experimentation**: If a feature experiment fails, simply deleting or abandoning the Git branch discards the associated database state.

## The Sync Loop

Collaboration in `caqui` mirrors a standard Git workflow, but applies to your entire application state.

### 1. Committing Changes
When you are ready to share your work, you commit your schema changes and the binary state of the database simultaneously.
```bash
caqui git add .
caqui git commit -m "Update schema and insert initial configuration data"
```

### 2. Fetching and Merging
To receive changes from others, you must explicitly fetch and merge. 

**Expert Note**: Do not use `caqui git pull`. The CLI proxy specifically intercepts the `merge` subcommand to inject the `-s sqlitevfs` strategy. `pull` will fail to use the custom driver and result in standard Git binary conflicts for `app.db`.

```bash
caqui git fetch origin main
caqui git merge origin/main
```
During the merge, if both you and your teammates have modified the database, `caqui` automatically resolves the conflicts at the data level without binary file corruption.

### 3. Pushing
Once merged and resolved, you push your reconciled state back to the remote.
```bash
caqui git push origin main
```

## Expert: The `caqui git` Proxy

Because Git is designed for text files, binary database files (`app.db`) traditionally cause unresolvable merge conflicts. To solve this, `caqui` provides a specialized proxy command: `caqui git`.

### The `sqlitevfs` Merge Driver
When you run `caqui git merge`, the CLI intercepts the command and automatically enforces the custom `-s sqlitevfs` merge strategy.

This driver intercepts the standard Git merge process. Instead of treating `app.db` as an opaque binary blob, the driver connects to the internal SQLite VFS. It analyzes the specific row-level mutations made on both branches and safely reconciles them at the data level. 

*(Note: Attempts to override this merge strategy using standard Git flags are actively blocked by the CLI to prevent database corruption).*

### Structural Diffs
Standard `git diff` on a SQLite file yields useless binary output. 

When you run `caqui git diff`, the proxy intercepts the command and invokes a custom internal diffing engine. It outputs a human-readable structural diff, highlighting exact schema alterations and potential state conflicts, allowing developers to review database changes as easily as code.

## Deployment via Git

By unifying code, schema, and data into a single Git repository, `caqui` unlocks powerful deployment patterns:

- **Local-First Apps:** Users can work completely offline, storing data locally, and use `caqui git sync` mechanics when a connection is restored.
- **Edge Functions:** Deploy a unique, fully synchronized database instance to every edge node simply by pulling the latest Git commit.
- **Ephemeral Environments:** Instantly spin up a full-stack preview environment (schema + populated data) just by cloning a repository or checking out a pull request branch.
