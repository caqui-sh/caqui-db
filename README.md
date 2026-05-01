# caqui

`caqui` is a high-performance, schema-driven SQLite database and API engine. It replaces the traditional multi-tier web stack (database, migration CLI, ORM, and API layer) with a **single executable binary**.

Simply write your schema, start the server, and instantly query your database via a powerful HTTP JSON API.

## Features

- **Single Binary**: No need to manage separate database, ORM, and API services.
- **Schema-Driven**: Define your models in a simple DSL and let `caqui` handle the rest.
- **Instant JSON API**: Automatically get a full CRUD HTTP JSON API based on your schema.
- **Decentralized Concurrency**: Operates on unique local copies of the repository and database, merged via a custom `git-merge-sqlitevfs` driver.

## Example

### 1. The Schema (`schema.cq`)
```
model User {
  id    ID     @id
  name  String
  email String @unique
  posts [Post]
}

model Post {
  id       ID     @id
  title    String
  content  String
  author   User   @relation
}
```

### 2. The JSON API Query

**Request:**
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
        "select": { "title": true }
      }
    }
  }'
```

**Response:**
```json
{
  "data": [
    {
      "id": "1",
      "name": "Alice",
      "posts": [
        { "title": "Hello World" },
        { "title": "Caqui is awesome" }
      ]
    }
  ]
}
```

## Documentation

### 1. Core Concepts
- **Schema DSL** (`./docs/schema.md`)
- **Primitive Types**

### 2. Relational Data
- **Relationships** (`./docs/relations.md`)

### 3. API Reference
- **Queries** (`./docs/queries.md`)
- **Mutations** (`./docs/mutations.md`)
- **Errors** (`./docs/errors.md`)

### 4. CLI & Workflows
- **Prototyping & Migrations** (`./docs/migrations.md`)
- **Version Control** (`./docs/workflow.md`)
- **CLI Commands** (`./docs/cli.md`)
- **Configuration** (`./docs/configuration.md`)

### 5. Quickstart
- **Quickstart Guide** (`./docs/quickstart.md`)

---

## Supported Platforms

⚠️ **Note:** Windows is strictly **not supported**.

`caqui` is officially supported exclusively on the following platforms:
- **macOS (ARM / Apple Silicon)**
- **Linux (ARM64)**
- **Linux (x86_64)**

---

> **For Contributors:** To understand the internal architecture or build from source, read **[DEVELOPER.md](./DEVELOPER.md)**.
