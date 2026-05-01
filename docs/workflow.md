# Decentralized Git Workflow

`caqui` uses a decentralized concurrency model. Instead of scaling a single central database server, **each user or client operates on their own unique local copy of the repository and database.**

## The Concept

In a traditional web stack, the database is a central bottleneck. `caqui` flips this model:
1. The source of truth is a **Git repository**.
2. The database (`app.db`) is part of that repository.
3. Every developer or environment has a local instance of the `caqui` engine and the database.

## The Sync Loop

Collaboration in `caqui` follows a standard Git flow:

### 1. Working Locally
You run the `caqui` API and make changes (mutations). These changes are immediately applied to your local `app.db`.

### 2. Committing Changes
When you are ready to share your work, you commit your schema changes and the binary state of the database.
```bash
caqui git add .
caqui git commit -m "Update schema and data"
```

### 3. Pulling and Merging
To receive changes from others, you pull from the remote repository.
```bash
caqui git pull origin main
```
During a pull, `caqui git merge` (and related commands) automatically resolve database conflicts safely at the data level without binary file corruption.

### 4. Pushing
Once merged, you push your reconciled state back to the remote.
```bash
caqui git push origin main
```

## Deployment & Hosting

`caqui` is ideal for decentralized environments:
- **Local-First Apps:** Users can work offline and sync when they have a connection.
- **Edge Functions:** Deploy a unique database instance to every edge node.
- **Ephemeral Environments:** Instantly spin up a full-stack environment by just cloning a repository.
