# caqui

`caqui` is a high-performance, schema-driven SQLite database and API engine. It replaces the traditional multi-tier web stack (database, migration CLI, ORM, and API layer) with a **single executable binary**.

Simply write your schema, start the server, and instantly query your database via a powerful GraphQL-style HTTP JSON API.

> **For Contributors and Developers:** Want to understand the internal architecture, zero-overhead networking, or how to build `caqui` from source? Please read **[DEVELOPER.md](./DEVELOPER.md)**.

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

⚠️ **Note:** Full concurrent multi-writer support within a *single* local instance via the custom Git VFS is currently a **planned feature** and is not yet fully supported. While the local engine is configured with Write-Ahead Logging (WAL) and `busy_timeout` pragmas to handle contention gracefully, heavy parallel write operations on a single local database may still encounter locking limitations.

---

## Getting Started

`caqui` uses a single schema file (`schema.cq`) to define your database tables, relationships, and API security rules. The executable exposes four simple commands to manage your lifecycle:

### 1. Initialize a Project (`caqui init`)
Generate a starter `schema.cq` file in your current directory.

```bash
caqui init
```

### 2. Prototype (`caqui db-push`)
Quickly sync your `schema.cq` to your local SQLite database. This instantly generates and applies the structural differences to your database (`app.db`). Best used during local development.

```bash
caqui db-push
```

### 3. Safe Migrations (`caqui migrate-dev`)
The safe, historical deployment workflow. This command reads your `schema.cq`, compares it to your previous migration files in `/migrations/`, and writes a new `_auto_migration.sql` script to disk before safely applying it to your live database.

```bash
caqui migrate-dev
```

### 4. Start the API Server (`caqui api start`)
Starts the database connection pool and mounts the universal dynamic execution router to `http://0.0.0.0:4000`.

```bash
caqui api start
```

---

## Writing your Schema (`schema.cq`)

`caqui` uses a strict, declarative, Prisma-inspired DSL. 

### Models and Primitive Types
Define your tables and basic types (`String`, `Int`, `Float`, `Boolean`).

```prisma
model User {
  id    String  @id @default(uuid())
  age   Int     @default(18)
  score Float
  admin Boolean
}
```

### Arrays
Store lists of primitives natively without needing secondary tables.

```prisma
model TagGroup {
  id   String   @id
  tags String[] 
}
```

### Relationships (1:N and N:M)
Link your models together. `caqui` automatically tracks and resolves deeply nested relationships.

```prisma
model Post {
  id       String  @id
  authorId String
  author   User    @relation(fields: [authorId], references: [id], onDelete: Cascade)
}
```

### Polymorphic Unions
Return multiple different models under a single dynamic property.

```prisma
union SearchResult = Post | User

model Query {
  id      String       @id
  results SearchResult 
}
```

### Attributes Reference
Customize your fields using the following attributes:

- `@id`: Marks the field as the Primary Key. 
- `@default(autoincrement())`: Automatically increments integer IDs.
- `@default(uuid())`: Automatically generates a sequential UUIDv7 on insert.
- `@default(now())`: Automatically sets the current timestamp on insert.
- `@default("static_value")`: Provides a static default value.
- `@unique`: Ensures all values in this column are globally unique.
- `@@unique([field1, field2])` / `@@index([field1, field2])`: Block-level composite index generation.
- `@updatedAt`: Automatically updates the timestamp whenever the row is modified.
- `@map("physical_name")`: Maps a clean API name (e.g., `firstName`) to a legacy database column (e.g., `usr_frst_nm`).
- `@ignore`: Prevents the API from ever returning this column (e.g., used for password hashes or internal data).

---

## Querying the API

`caqui` exposes a single, universal HTTP POST endpoint at `/api/v1/query`.

Instead of writing custom backend routes, you request the exact shape of the data you want. `caqui` will automatically fetch it, including nested relationships, in a single, highly-optimized query.

### Example: Fetching deeply nested data

```bash
curl -X POST http://localhost:4000/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "model": "User",
    "action": "findMany",
    "select": {
        "id": true,
        "name": true,
        "posts": {
            "select": {
                "title": true,
                "authorId": true
            }
        }
    }
  }'
```

If your schema is valid, `caqui` returns exactly what you asked for, fully formatted as a JSON graph.graph.