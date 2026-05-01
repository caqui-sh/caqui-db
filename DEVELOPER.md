# caqui Architecture

This document provides a deep, technical dive into the internals of the `caqui` engine. It is intended for developers, contributors, and systems engineers who want to understand how the engine achieves its "Zero-Overhead" operational footprint.

## The Zero-Overhead Philosophy

`caqui` was designed to challenge the established norms of the modern backend web stack. Classical architectures depend on distinct, network-separated boundaries:
1. **The Database Server**: A standalone daemon (e.g., PostgreSQL, MySQL) listening on a TCP/IP port.
2. **The API Layer**: A Node.js, Go, or Python server that maintains connection pools.
3. **The ORM**: An Object-Relational Mapper that translates code into SQL, usually suffering from N+1 query inefficiencies.
4. **The Migration CLI**: A separate tool tracking `.sql` scripts.

`caqui` collapses these four boundaries into a **single, statically linked executable** utilizing an embedded SQLite engine.

### Bypassing Network Latency
There is no TCP/IP overhead. Database queries are executed within the exact same process space as the HTTP router. `caqui` intercepts the host's file descriptors via a custom C-FFI SQLite Virtual File System (VFS), allowing I/O to execute locally at the maximum speed of the host's RAM and disk controllers.

### Bypassing the N+1 Problem
Traditional ORMs fetch parent records, allocate memory structs, and then loop over children (the N+1 problem) or use massive `LEFT JOIN` Cartesian products that exponentially inflate network transit bytes. 

`caqui` shifts the entire burden of object hydration and JSON mapping down into the `SQLite` VDBE (Virtual Database Engine) using deeply nested Correlated Subqueries coupled with SQLite's `json_group_array()` and `json_object()` functions. The database returns exactly one mathematically perfect string containing the entire graph.

### Bypassing Serialization Overhead (Zero Code Generation)
Because the `SQLite` engine natively formats the JSON payload inside the C layer, the Rust HTTP handler does not allocate thousands of structs to deserialize SQL rows. It receives a single byte-string buffer from the database and proxies it immediately into an `application/json` HTTP response. There are zero generated TypeScript or Rust types to manage.

---

## Streamlined Developer Workflows (Cargo Tasks)

When building or modifying `caqui` from source, we utilize native Cargo aliases defined in `.cargo/config.toml` to streamline local development (similar to `deno task` or `npm run`).

Execute the following commands from the root directory:

- **`cargo setup`**
  Initializes a new project. Invokes `cargo run --release --bin caqui -- init`. Generates a default `schema.cq` file in the current directory to bootstrap development.

- **`cargo push`**
  Rapid prototyping workflow. Invokes `cargo run --release --bin caqui -- push`. Bypasses the shadow database history and directly compares the `schema.cq` file against the live physical database (`app.db`). Generates and executes the structural SQL differences instantly.

- **`cargo migrate`**
  The safe deployment workflow. Invokes `cargo run --release --bin caqui -- migrate`. Boots a transient, ephemeral "Shadow Database" in memory. It plays the existing `/migrations/*.sql` history files, diffs the historical state against your current `schema.cq`, and writes an automated `_auto_migration.sql` file safely to disk before executing it on your live database.

- **`cargo start`**
  Initializes the engine. Invokes `cargo run --release --bin caqui -- api start`. Bootstraps the custom Virtual File System (VFS), creates the SQLite connection pool, and mounts the universal dynamic execution router to `http://0.0.0.0:4000`.

---

## The 5-Phase Execution Pipeline

The core binary orchestrates five distinct sub-systems in milliseconds upon boot.

### Phase 1: The Custom Virtual File System (VFS)
`crates/engine-core`
SQLite allows developers to override its low-level OS interface. Before any connections are opened, `caqui` bootstraps a custom VFS via C-FFI. This layer intercepts file locking, journaling, and memory mapping. By controlling the VFS, `caqui` ensures that asynchronous `Tokio` thread workers do not fatally collide with SQLite's synchronous C-locks.

**Concurrency Note:** Full multi-writer concurrency support within this custom VFS is currently a **planned enhancement**. While we enforce `journal_mode=WAL` and `busy_timeout=5000` to maximize safety and throughput, the current VFS implementation is optimized for single-writer consistency. Deep concurrent write support is a priority for the next major phase of development.

During connection pool bootstrapping (`deadpool-sqlite`), `caqui` also injects native Rust closures directly into the SQLite runtime. For example, `@default(uuid())` is handled by compiling a Rust `uuid::Uuid::now_v7()` generator closure into the database connection, exposing it natively inside SQL expressions.

### Phase 2: The Schema Parser and DSL Engine
`crates/schema-parser`
`caqui` parses a strict Prisma-inspired DSL via a zero-allocation `pest` PEG parser. The text is lexicalized and converted into a thread-safe, immutable Abstract Syntax Tree (AST). The parser strictly categorizes relational linkages, primitive scalar arrays, and Polymorphic Unions, and enforces rule sets (e.g., validating that an `@updatedAt` tag isn't applied to a string).

### Phase 3: The DBA Diffing Engine
`crates/schema-mapper`
This phase provides declarative automated migrations. 
- **Introspection**: The engine interrogates the active physical database via `PRAGMA table_info`, evaluating column types, nullable states, and default bounds. It constructs a "Live IR" (Intermediate Representation).
- **Delta Computation**: It diffs the "Live IR" against the AST's "Desired IR" $O(N)$.
- **Safe Execution**: SQLite cannot dynamically alter table column properties safely. If the engine detects a restrictive type change or a newly added `NOT NULL` constraint, it generates an atomic 12-step table rebuild sequence (`PRAGMA foreign_keys=OFF` -> `BEGIN TRANSACTION` -> `CREATE _new_tbl` -> `INSERT SELECT` -> `DROP` -> `RENAME` -> `COMMIT`). 

During the `cargo migrate` workflow, this logic runs against a transient, in-memory **Shadow Database**, playing historical `.sql` scripts to ascertain safety before touching the developer's live disk.

### Phase 4: The Query Compiler
`crates/query-compiler`
This subsystem dynamically bridges the gap between dynamic JSON requests and the database. 
- **Recursive Traversals**: Nested relationship queries are transformed into recursive Correlated Subqueries natively aliasing down the tree (`t0`, `t1`, `t2`).
- **Array Mutators**: `String[]` operations are mapped to SQLite JSON1 paths (e.g., `UPDATE tbl SET tags = json_insert(COALESCE(tags, '[]'), '$[#]', 'value')`).
- **Dynamic Discriminators**: Polymorphic Unions (`union Result = A | B`) are elegantly resolved per row. The compiler generates native SQLite `CASE` statements that evaluate the hidden `{field}_type` discriminator columns, forcing the database to execute $O(1)$ subqueries against the target table *only* when the row matches, bypassing massive sparse `JOIN` overheads.

### Phase 5: The Dynamic API & HTTP Layer
`crates/api-layer`
Built heavily on `Axum` and `Tokio`. 
Incoming dynamic HTTP JSON payloads are evaluated Just-In-Time (JIT) against the AST. If a user requests a field that doesn't exist, the router returns a fast $O(1)$ Hash Map rejection. 
It acts as a physical barrier implementing the **Security Interceptor**—if a user requests a column tagged with `@ignore` (like password hashes), the API layer throws a Security Exception before the SQL query is even compiled, ensuring absolute boundary protection.

When validated, the payload is compiled by Phase 4, thrown into the thread-safe `deadpool-sqlite` background worker pool via `.interact()` (to prevent starving `Tokio` async workers), and the returned JSON bytes are pushed out to the HTTP client natively.

---

## Future Enhancements

To maintain feature parity with modern DSLs (like Prisma or GraphQL), the following architectural features are under consideration for future development:

### 1. Composite Unique Constraints & Indexes
- **Syntax:** `@@unique([firstName, lastName])`
- **Use Case:** Highly common in join tables (e.g., `@@unique([userId, postId])` for a `Like` tracking table). Our schema parser currently handles field-level `@unique`, but block-level composite unique enforcement is needed for complex relationships.

### 2. Database Mapping (`@map` / `@@map`)
- **Syntax:** `@@map("tbl_users")` or `@map("first_name")`
- **Use Case:** Allows developers to expose clean, idiomatic camelCase names in their API and JSON responses while physically interacting with a legacy database that enforces snake_case or prefix-based schemas.

### 3. Composite Foreign Keys
- **Syntax:** `@relation(fields: [f1, f2], references: [r1, r2])`
- **Use Case:** Required for interacting with complex legacy databases that use composite primary keys. The AST `fields` and `references` vectors already natively support multiple values, but the SQL generation and runtime Query Compiler `JOIN` logic needs expansion to support parsing arrays of keys.

### 4. Batch Operations
- **Syntax:** `action: "createMany"`, `action: "updateMany"`, `action: "deleteMany"`
- **Use Case:** High-performance bulk data modifications. Requires query chunking to circumvent SQLite's parameter limits and logic to return aggregate counts (`{ count: N }`) instead of materializing thousands of objects into application memory.

### 5. Advanced AST Field Types
- **Enums:** Native support for schema enumeration types (e.g., `enum Role { ADMIN, USER }`).
- **JSON / JSONB:** A dedicated JSON scalar for structured payloads, allowing for native database JSON operations.
- **Bytes / Binary Data:** A scalar type for `BLOB` / binary storage (e.g., images, file buffers).
- **High-Precision Numerics:** Support for `Decimal` and `BigInt` for exact financial calculations or extremely large counters.
- **Embedded Documents:** Native sub-object definitions common in NoSQL schemas, allowing nested structures without separate tables.