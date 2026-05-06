# Mutations API

Caqui provides a deeply-nested, JSON-based Mutations API that enables you to manipulate your data graph with precision. All mutations are resolved using $O(1)$ routing speed and mapped into zero-latency execution plans.

## Available Actions

The mutation surface area is split into **Singular Actions** (targeting individual records) and **Batch Actions** (targeting multiple records matching criteria).

### Singular Actions
Singular actions are designed for precise, atomic operations on specific records.
- **`create`**: Insert a new record into the database.
- **`update`**: Modify an existing record based on its unique identifier.
- **`delete`**: Remove a record.
- **`connect`**: Link two existing records via a relational field.
- **`disconnect`**: Unlink two existing records.

### Batch Actions
Batch actions allow you to operate on sets of records dynamically.
- **`updateMany`**: Update multiple records matching a specific `where` clause.
- **`deleteMany`**: Delete multiple records matching a specific `where` clause.

### Array Sub-Actions
For fields defined as scalar arrays (e.g., `String[]` or `Int[]`), Caqui provides atomic modifiers to mutate the list directly within an update payload. These modifiers accept either a single value or an array of values:
- **`push`**: Append new element(s) to the end of the array.
- **`pull`**: Remove all occurrences of the specified scalar value(s) from the array.
- **`pullIndex`**: Remove element(s) at the specific numeric index/indices.

*Example:* 
```json
{
  "action": "update",
  "model": "Article",
  "where": { "id": "1" },
  "data": {
    "tags": { "push": ["graphql", "rust"] }
  }
}
```

### Relational Sub-Actions
When updating a parent record, you can nest actions to mutate its relationships in the same transaction. All the Singular and Batch actions (`create`, `connect`, `disconnect`, `update`, `delete`, `updateMany`, `deleteMany`) can be used as nested sub-actions.

For "to-many" relationships, an additional modifier is available:
- **`set`**: Replaces the entire list of connected records. This atomically disconnects all currently linked records and connects only the ones provided in the `set` payload.

*Example:* 
```json
{
  "action": "update",
  "model": "Post",
  "where": { "id": "1" },
  "data": {
    "authors": { 
      "set": [{ "__id": "id1" }, { "__id": "id2" }] 
    }
  }
}
```


---

## Limitations

While the Mutations API is highly flexible, it does enforce certain structural and execution constraints to guarantee performance and predictability:

### Relational Constraints Inside Batch Actions
When performing a batch update action (`updateMany`), the engine applies specific restrictions to nested relational mutations to guarantee execution predictability and prevent ambiguous semantics.

For **forward relations** (where the child schema physically holds the foreign key, often the "many" side of a 1-to-many relationship):
- You **cannot** nest `connect` or `set` actions inside an `updateMany`. Attempting to connect a single child record to multiple distinct parents in a single bulk operation violates relational integrity (as a single scalar foreign key can only point to one parent at a time). Note: Even in Many-to-Many (M:N) relationships—where connecting multiple parents and children is structurally possible via a join table—Caqui's execution engine currently applies this same restriction at the bulk processing layer. If you need to batch connect or set in M:N scenarios, you must iterate with singular `update` mutations or update the target children directly. The engine will reject these bulk operations with a Semantics Error.

  *❌ Invalid Payload:*
  ```json
  {
    "action": "updateMany",
    "model": "User",
    "where": { "status": "ACTIVE" },
    "data": {
      "posts": { "connect": { "id": "post-123" } } // A single post cannot belong to all ACTIVE users
    }
  }
  ```

- You **can** nest a `create` action. The engine will safely generate exactly *one* standalone child record and broadcast its ID, linking all matched parents to that single new child.

For **reverse relations** (where the parent being updated holds the foreign key, often the "1" side of a 1-to-many relationship):
- You **cannot** nest singular actions like `update`, `connect`, or `disconnect` as they would introduce race conditions or conflicting single-target writes.

  *❌ Invalid Payload:*
  ```json
  {
    "action": "updateMany",
    "model": "Post",
    "where": { "published": false },
    "data": {
      "author": { "update": { "data": { "reputation": 10 } } } // Ambiguous if multiple posts try to update the same author simultaneously
    }
  }
  ```

- If you need to perform relational mutations alongside batch updates on reverse relations, you must use nested batch actions (e.g., nesting an `updateMany` inside another `updateMany`) or perform them in a separate API call.

### Array Field Constraints
When performing mutations on fields, the engine strictly delineates between native scalar arrays and relational collections:
- **Array Modifiers (`push`, `pull`, `pullIndex`)**: These are strictly reserved for native scalar arrays (e.g., `String[]` or `Int[]`). Attempting to use them on relational record lists will result in a Validation Error.

  *❌ Invalid Payload:*
  ```json
  {
    "data": {
      "comments": { "push": { "text": "Great!" } } // Cannot use `push` on a relational field, use `create` instead
    }
  }
  ```

- **Relational Array Creation on FK-Holding Fields**: You cannot perform a nested array `create` action (e.g., `create: [{...}, {...}]`) on a relation where the parent physically holds the foreign key (structurally a Many-to-One or One-to-One relation). The engine actively blocks treating scalar relation fields as collection fields.

  *❌ Invalid Payload:*
  ```json
  {
    "data": {
      "author": { "create": [{ "name": "Alice" }, { "name": "Bob" }] } // A `Post` only has one `authorId` column, cannot create an array of authors
    }
  }
  ```
### Polymorphic Field Actions
When performing a nested mutation directly on a polymorphic relation (a field typed as a Union or Base Shape), the engine restricts the available inline actions. 
- You **can** perform nested `connect` and `create` actions.
- You **cannot** perform nested `update`, `delete`, or `disconnect` actions inline without explicitly using `__kind` inside the `where` block to disambiguate the target.
- You **cannot** perform the `set` action *at all* on nested polymorphic relations. Safely replacing an entire set of abstract polymorphic links across disparate concrete tables in a single atomic SQL transaction is currently unsupported. You must explicitly `disconnect` and `connect` instead.

  *❌ Invalid Payload:*
  ```json
  {
    "data": {
      "media": { "set": [{ "__kind": "Video", "__id": "v1" }, { "__kind": "Image", "__id": "i1" }] } // Nested `set` is blocked on polymorphic relation `media`
    }
  }
  ```
---

## Abstract Base Shapes & Polymorphism

Caqui features a sophisticated polymorphic execution engine that treats abstract base shapes differently depending on the context of the mutation.

### Singular Mutations on Base Shapes
When performing singular actions (`create`, `update`, `delete`, `connect`, `disconnect`), you **cannot** mutate an abstract base shape directly. The engine actively prevents you from doing so.

*❌ Invalid Payload:*
```json
{
  "action": "update",
  "model": "Content", // Content is a Base Shape, not a physical table!
  "where": { "id": "1" },
  "data": { "status": "PUBLISHED" }
}
```

To maintain optimal execution speed and prevent the engine from having to "guess" the target table, you must explicitly disambiguate which concrete table you are targeting.

**Example: Updating a Polymorphic Field**
Use the `__kind` attribute in your payload to explicitly resolve the target:
```json
{
  "update": {
    "where": { "__kind": "Article" },
    "data": { "status": "PUBLISHED" }
  }
}
```

### Batch Mutations on Base Shapes
Batch actions (`updateMany`, `deleteMany`), on the other hand, **fully support** targeting abstract base shapes directly at any depth. 

When you issue a batch command against an abstract base, Caqui intelligently and automatically fans out the SQL execution to every single concrete descendant model, broadcasting the mutation into a single atomic transaction.

**Example: Broadcasting an UpdateMany**
```json
{
  "favorites": {
    "updateMany": [
      {
        "where": { "visibility": "DRAFT" },
        "data": { "visibility": "PUBLISHED" }
      }
    ]
  }
}
```
*If `favorites` points to an abstract base shape (e.g., `Content`), the engine will seamlessly resolve the subqueries for `Article`, `Video`, and any other inheriting models.*

---
