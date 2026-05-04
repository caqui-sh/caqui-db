# Architectural Blueprint: Nested Mutations & Batch Operations

## Overview
This document outlines the architectural research, limitations, and envisioned design for implementing Nested Mutations (`update`, `delete`) and Batch Operations (`updateMany`, `deleteMany`, `createMany`) within the `caqui` engine.

## 1. The Current State
Currently, `caqui` handles nested array relationships (1:N) and polymorphic arrays by allowing the mutation of the **linkage** or the **creation** of new records, but it lacks the ability to deeply modify existing nested data.

For a standard relation (`posts: Post[]`):
- `create`: Inserts new records and links them.
- `connect`: Links existing records.
- `disconnect`: Unlinks existing records.
- `set`: Replaces all links.

For a **Polymorphic** relation (`activities: Content[]` where `Content` is `Article | Video`):
The payload must be wrapped in the concrete model name to disambiguate the target table:
```json
"activities": {
  "Article": { "create": [{ "body": "..." }] },
  "Video": { "connect": [{ "__id": "vid_1" }] }
}
```
**Limitation:** It is not currently possible to issue an `update` or `delete` inside nested blocks. These operations must be performed via root-level queries.

## 2. Envisioned Design: Nested Update & Delete

The primary architectural goal for nested updates and deletes is **Scoped Security**. If a client attempts to delete a `Post` through a `User`'s relation array, the engine must silently inject a structural constraint (e.g., `WHERE authorId = 'user_id'`) to ensure the user cannot maliciously mutate a record that belongs to a different parent.

### Standard Syntax
```json
{
  "model": "User",
  "action": "update",
  "where": { "__id": "u1" },
  "data": {
    "posts": {
      "update": [
        { "where": { "__id": "p1" }, "data": { "title": "New Title" } }
      ],
      "delete": [
        { "__id": "p2" } // Safely deletes p2 ONLY if it belongs to u1
      ]
    }
  }
}
```

### The Polymorphic Challenge & `__kind` Discriminator
Nested updates and deletes within polymorphic arrays pose a unique challenge: the engine must know which physical SQLite table to target without incurring the massive latency of "Look-Before-Write" reflection queries.

Furthermore, because UUID/CUIDs are generated per-table, there is a theoretical (though mathematically improbable) chance of primary key collisions across different concrete models implementing the same Union or Base.

**Solution:** Rather than strictly wrapping the entire mutation payload in the model name (which creates bulky, complex JSON structures), we rely on the client providing the `__kind` discriminator field directly within the `where` or `data` block.

Because `caqui` automatically injects the `__kind` field into every model and returns it on read queries, the client already possesses this exact string. By requiring it during polymorphic mutations, the compiler can instantly and safely route the SQL `UPDATE` or `DELETE` to the correct table with zero ambiguity.

```json
{
  "model": "Collection",
  "action": "update",
  "where": { "__id": "col_1" },
  "data": {
    "items": {
      "update": [
        { 
          "where": { "__id": "art_1", "__kind": "Article" }, 
          "data": { "body": "Revised" } 
        }
      ],
      "delete": [
        { "__id": "vid_3", "__kind": "Video" }
      ]
    }
  }
}
```
**Architectural Benefits:**
1. **Flat, Ergonomic API:** The payload remains a flat array of operations (`update: [...]`), exactly like standard non-polymorphic relations.
2. **Zero Ambiguity:** The compiler knows immediately which AST `ModelNode` to validate the `data` against.
3. **Collision-Proof:** The `__kind` guarantees we never accidentally update an `Article` if a `Video` happens to share the same `__id`.
4. **Zero-Latency Execution:** The compiler generates a single, direct SQL statement (`UPDATE Article SET...`) without any LBW queries.

## 3. Batch Operations (`updateMany` / `deleteMany`)
Batch operations replace specific `__id` targeting with a generic `where` filter, enabling high-performance bulk operations.

### Batch Polymorphism & AST-Driven Fan-Out
When dealing with polymorphic relationships, batch operations (`updateMany`, `deleteMany`) require a different approach than singular operations. If a relation points to a `Base` (e.g., `Content` extended by `Article` and `Video`), forcing the client to specify a concrete `__kind` destroys the abstraction the Base is meant to provide.

**AST-Driven Fan-Out:**
Because the engine parses the entire schema into an AST at boot, it knows exactly which concrete models implement a specific `Base`. When a client issues a batch operation against a Base relation:
1. The compiler validates the `where` and `data` blocks against the Base's defined fields.
2. The compiler "fans out" and generates an array of SQL statements—one for each concrete implementing table.
3. All statements are executed within the same transaction. If a specific table has no matching records, the operation safely affects 0 rows.

### Implicit Base Marker Filtering
To provide extreme granularity within these fan-out operations, clients can utilize the implicit Base marker fields (`__BaseName: true`) that `caqui` automatically injects into polymorphic models.

Because a model can inherit from multiple Bases (e.g., `Article extends Content, Timestamped`), the client can issue an `updateMany` that targets *any* polymorphic array, but narrow the fan-out dynamically:

```json
{
  "model": "Collection",
  "action": "update",
  "where": { "__id": "col_1" },
  "data": {
    "items": {
      "updateMany": {
        "where": { 
           "__Timestamped": true, 
           "published": false 
        },
        "data": { "published": true }
      }
    }
  }
}
```
**How the Compiler uses Markers:**
If `__Timestamped: true` is included in the `where` block, the AST compiler can heavily optimize the fan-out. Instead of generating `UPDATE` statements for *every* model in the `items` union/base, it cross-references the AST and only generates SQL for the concrete models that physically implement the `Timestamped` base. 

This creates a highly expressive, type-safe, and deeply optimized batch mutation capability without sacrificing the abstraction of polymorphism.
```json
{
  "model": "Post",
  "action": "updateMany",
  "where": { "views": { "lt": 10 } },
  "data": { "status": "ARCHIVED" }
}
```

### Nested Batch Operations (Relational Scoping)
Nested batch operations are extremely powerful, allowing bulk modifications that are strictly scoped to the parent record.
```json
{
  "model": "User",
  "action": "update",
  "where": { "__id": "u1" },
  "data": {
    "posts": {
      "updateMany": {
        "where": { "published": false },
        "data": { "published": true }
      },
      "deleteMany": {
        "where": { "spam": true }
      }
    }
  }
}
```
*Internally, `updateMany` compiles to:* `UPDATE Post SET published = 1 WHERE authorId = 'u1' AND published = 0;`

## 4. Primitive Arrays (`String[]`, `Enum[]`)
Primitive arrays are stored in SQLite using JSON1. `caqui` currently supports `push`. To achieve full parity, we need a `pull` operator.

```json
"tags": { "push": "new_tag" }
"tags": { "pull": "outdated_tag" }
```
**Implementation Note:** `pull` will likely require utilizing SQLite's `json_remove` in conjunction with `json_each` to match and extract literal values dynamically.

## 5. Summary of Required Engine Work
To fully realize this architecture, the following internal sub-systems require expansion:

1. **`query-compiler` (AST & Parsing):**
   - Extend the AST mutator payloads to accept `update`, `delete`, `updateMany`, and `deleteMany` blocks.
   - Support `WhereClause` AST nodes within mutation payloads.
2. **`api-layer` (Execution Engine):**
   - Implement `ExecutionStep::UpdateBranch` and `ExecutionStep::DeleteBranch` to handle scoped, single-record relational targeting.
   - Implement `ExecutionStep::UpdateMany` and `ExecutionStep::DeleteMany`, injecting the parent's `__id` into the parameterized `WHERE` clause at runtime.
   - Implement SQLite `json_remove` bindings for the `pull` array operator.