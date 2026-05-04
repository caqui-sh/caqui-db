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

### The Polymorphic Challenge
Nested updates and deletes within polymorphic arrays pose a unique challenge: the engine must know which physical SQLite table to target without incurring the massive latency of "Look-Before-Write" reflection queries.

**Solution:** We extend the existing disambiguation pattern. By requiring the client to wrap the action in the concrete model name, the compiler can generate direct, secure, and isolated `UPDATE` and `DELETE` statements, preserving the high-performance linear execution plan.

```json
{
  "model": "Collection",
  "action": "update",
  "where": { "__id": "col_1" },
  "data": {
    "items": {
      "Article": {
        "update": [ { "where": { "__id": "art_1" }, "data": { "body": "Revised" } } ],
        "delete": [ { "__id": "art_2" } ]
      },
      "Video": {
        "delete": [ { "__id": "vid_3" } ]
      }
    }
  }
}
```

## 3. Batch Operations (`updateMany` / `deleteMany`)
Batch operations replace specific `__id` targeting with a generic `where` filter, enabling high-performance bulk operations.

### Root-Level Batch Operations
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