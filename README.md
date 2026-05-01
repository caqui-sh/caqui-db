# caqui

`caqui` is a high-performance, schema-driven SQLite database and API engine. It replaces the traditional multi-tier web stack (database, migration CLI, ORM, and API layer) with a **single executable binary**.

Simply write your schema, start the server, and instantly query your database via a powerful GraphQL-style HTTP JSON API.

## Supported Platforms

⚠️ **Note:** Windows is strictly **not supported**.

`caqui` is officially supported exclusively on the following platforms:
- **macOS (ARM / Apple Silicon)**
- **Linux (ARM64)**
- **Linux (x86_64)**

---

## Concurrency & Performance Status

The concurrency model of `caqui` is decentralized. Instead of scaling a single active database connection pool across thousands of parallel web requests, **each user/client operates on their own unique local copy of the repository and database.** 

Users make changes in their isolated environments and push those changes to a remote repository, where the custom `git-merge-sqlitevfs` driver intelligently reconciles and merges the SQLite state globally.

---

## Quick Start (5 Minutes)

### 1. Initialize
Generate a starter `schema.cq` file.

```bash
caqui init
```

### 2. Define your Schema
Edit `schema.cq` to define your models.

```prisma
model User {
  id    String @id @default(uuid())
  name  String
  posts Post[]
}

model Post {
  id       String @id @default(uuid())
  title    String
  authorId String
  author   User   @relation(fields: [authorId], references: [id])
}
```

### 3. Push to Database
Apply your schema changes to the local SQLite database (`app.db`).

```bash
caqui schema db-push
```

### 4. Start the API
Launch the universal engine.

```bash
caqui api start
```

### 5. Query
Fetch your data via HTTP POST.

```bash
curl -X POST http://localhost:4000/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "model": "User",
    "action": "findMany",
    "select": { "id": true, "name": true }
  }'
```

---

## Detailed Documentation

For a comprehensive guide on all features, please refer to our modular documentation:

- 🏗️ **[Schema DSL Reference](./docs/schema.md)**: Models, types, relations, and attributes.
- 🔍 **[API Query Reference (Read)](./docs/queries.md)**: `findMany`, filtering, projections, and relational filters.
- ✍️ **[API Mutation Reference (Write)](./docs/mutations.md)**: `create`, `update`, `delete`, `upsert`, and nested writes.
- 💻 **[CLI Commands Reference](./docs/cli.md)**: Initialization, migrations, API server, and git proxy behaviors.
- 🔄 **[Decentralized Git Workflow](./docs/workflow.md)**: Understanding the sync loop and `sqlitevfs` merge driver.
- 📈 **[Database Lifecycle & Migrations](./docs/migrations.md)**: `db-push` vs `migrate-dev`, shadow databases, and table rebuilds.
- 🔗 **[Advanced Relational Rules](./docs/relations.md)**: Named relations, self-referential models, and deferrable constraints.
- 🚫 **[Error Handling & API Responses](./docs/errors.md)**: HTTP status codes, error shapes, and troubleshooting.
- ⚙️ **[Configuration & Engine Pragmas](./docs/configuration.md)**: Ports, foreign key enforcement, and custom SQLite functions.

---

> **For Contributors:** To understand the internal architecture or build from source, read **[DEVELOPER.md](./DEVELOPER.md)**.
