# Quickstart Guide

`caqui` is a high-performance, schema-driven SQLite platform designed for building consistent, transaction-safe APIs with zero overhead. This guide will help you move from a blank directory to a production-ready API in minutes.

## 1. Project Initialization

Initialize a new project environment to generate a default configuration and schema:

```bash
caqui init
```

This creates a `schema.cq` file in your root directory, which serves as the single source of truth for your database structure and API capabilities.

## 2. Professional Schema Design

Open `schema.cq` and define your data model. `caqui` supports advanced relational patterns, unique constraints, and polymorphic unions out of the box. Relations are implied by type signatures unless you need to override defaults.

```prisma
model User {
  name:    String
  email:   String @unique
  profile: Profile?
  posts:   Post[]
  @@id(uuid)
}

model Profile {
  bio:     String
  user:    User
  @@id(uuid)
}

model Post {
  title:   String
  author:  User
  @@id(uuid)
}

// Polymorphic Union for cross-model search
union SearchResult = User | Post
```

## 3. Database Synchronization Strategies

`caqui` provides two distinct workflows for keeping your database in sync with your schema.

### Rapid Prototyping (`push`)

During active development, use `push` for high-velocity iteration. It computes the diff between your schema and the database and applies it instantly without maintaining a migration history.

```bash
caqui schema push
```

### Safe Evolution (`migrate`)

 For production environments, use `migrate`. This generates versioned `.sql` migration files in a `/migrations` directory, providing a traceable history of your database evolution.

```bash
caqui schema migrate
```

## 4. Database as Code (`git` proxy)

`caqui` includes a built-in `git` proxy, allowing you to manage your database versioning and migration history directly through the CLI. This ensures that your schema and migrations are always synchronized with your source code.

```bash
caqui git commit -m "Add Profile model and relations"
```

## 5. High-Performance Execution

Start the unified engine to mount the dynamic API. By default, the engine binds to port `4000`, but this can be overridden via the `PORT` environment variable.

```bash
export PORT=4000
caqui api start
```

**Zero-Overhead Note**: The API is now running at `http://localhost:4000/api/v1/query`. Complex graph traversals are compiled into a single SQL query, neutralizing the N+1 problem.

## 6. Interacting with the API

Interact with the universal endpoint using a JSON payload. The following example demonstrates a deeply nested `findMany` query:

```bash
curl -X POST http://localhost:4000/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "model": "User",
    "action": "findMany",
    "where": {
      "email": { "eq": "alice@example.com" }
    },
    "select": {
      "name": true,
      "posts": {
        "select": { "title": true }
      },
      "profile": {
        "select": { "bio": true }
      }
    }
  }'
```
