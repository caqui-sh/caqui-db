# DataEngine

DataEngine is a high-performance, schema-driven SQLite platform that collapses the traditional multi-tier web stack (Database Server, Migrations CLI, Object-Relational Mapper, and HTTP API Layer) into a **single, statically linked executable**.

## Supported Platforms

⚠️ **Note:** Windows is strictly **not supported**.

DataEngine is heavily optimized for UNIX-like environments and is officially supported exclusively on the following platforms:
- **macOS (ARM/Apple Silicon)**
- **Linux (ARM64)**
- **Linux (x86_64)**

## Core Philosophy: The Zero-Overhead Engine

DataEngine bypasses the architectural limitations of classical backend ecosystems by eliminating unnecessary middle-men.

- **Zero Network Latency**: By bypassing TCP/IP and running SQLite locally via C-FFI, database I/O execution occurs at the speed of host RAM.
- **Zero N+1 Queries**: The custom Query Compiler shifts all object hydration and JSON mapping into the SQLite C-layer. Deeply nested, relational API queries are resolved using Correlated Subqueries and `json_group_array()`, completing infinite graph depths in exactly one mathematically perfect SELECT statement.
- **Zero Code Generation**: The API layer protects itself and resolves queries dynamically against a thread-safe, in-memory Abstract Syntax Tree (AST). There are zero generated TypeScript/Rust structs to manage, ensuring the binary remains hyper-lean and compilation times remain lightning fast.
- **Native Arrays and Polymorphism**: Primitive scalar arrays (e.g., `String[]`) are natively updated in C-memory via `json_insert`. Polymorphic Unions (e.g., `union SearchResult = Article | User`) are elegantly mapped using runtime $O(1)$ SQLite `CASE` resolution on physical discriminator columns.
- **Single Binary Deployment**: Leveraging the bundled feature of `rusqlite` and aggressive Link-Time Optimization (LTO), the custom Virtual File System (VFS), mathematical migration engine, query compiler, and Axum HTTP server are compiled into one standalone `.elf` or Mach-O executable.

## How it Works: The 5 Phases

DataEngine's architecture is composed of five specialized subsystems:

### 1. Custom C-FFI Virtual File System (VFS)
`crates/engine-core` intercepts standard SQLite OS-level I/O operations through a custom C-FFI Virtual File System interface, bootstrapping the underlying storage mechanism entirely within the rust process.

### 2. Schema Parser and DSL Engine
`crates/schema-parser` utilizes `pest` to tokenize a custom, Prisma-inspired declarative DSL (e.g., `schema.dsl`) with zero allocations, validating graph integrity and converting multidimensional arrays and polymorphic unions into a strict Abstract Syntax Tree (AST).

### 3. Schema Diffing and DBA Engine
`crates/schema-mapper` generates a mathematical "Live IR" by introspecting the active SQLite database using `PRAGMA table_info`. It diffs this against the AST's "Desired IR" to calculate strict `MigrationOp` states, safely orchestrating SQLite's atomic 12-step table rebuild sequence to execute automated migrations.

### 4. Query Compiler and Execution Engine
`crates/query-compiler` receives dynamic JSON requests and compiles them into a Query IR. It constructs nested relational SQL utilizing `json_group_array()` and native discriminator `CASE` resolution. `engine-core` then executes these massive string payloads, directly deserializing the single-row SQLite JSON response into the application without allocating intermediate memory structs.

### 5. Dynamic Custom API & HTTP Layer
`crates/api-layer` serves the Axum router. Incoming requests are JIT-hydrated against the shared, thread-safe memory AST. Malicious or undefined field requests are blocked instantly with an $O(1)$ lookup, and valid payloads are passed to the Query Compiler, streaming the raw SQLite bytes back out as an `application/json` HTTP response.

## Getting Started

### Prerequisites
- Rust 1.80+ 
- A supported UNIX-like OS (macOS ARM, Linux ARM, or Linux x86).

### Defining your Schema
Create a `schema.dsl` file in the root directory:

```prisma
model User {
    id String @id
    name String
    posts Post[]
}

model Post {
    id String @id
    title String
    author User
}

union SearchResult = User | Post
```

### CLI Workflows

The unified binary accepts commands via `clap` to manage the lifecycle of your database.

**1. Apply a non-destructive prototype diff to the database:**
```bash
cargo run --release -- DbPush
```

**2. Generate and run safe, historical `.sql` migration files via the Shadow DB:**
```bash
cargo run --release -- MigrateDev
```

**3. Boot the unified API server:**
```bash
cargo run --release -- Start
```
The server binds to `0.0.0.0:4000` and exposes a universal endpoint at `POST /api/v1/query`.

### Example Query

Once the server is running, you can request deeply nested relational graphs via a single HTTP request:

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
                "title": true
            }
        }
    }
  }'
```
The database executes exactly one query and returns the fully materialized object graph instantly.