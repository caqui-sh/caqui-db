# API Mutation Reference (Write)

`caqui` provides a high-performance, transaction-safe mutation engine designed for complex, nested write operations. A single mutation request can perform multiple operations across related models, ensuring atomicity through SQLite transactions.

All mutations are performed via the same `/api/v1/query` endpoint using the `POST` method.

## Root-Level Actions

Root-level mutations target a single primary record. If a `where` block matches multiple records, only the first record encountered is modified/deleted (though `where` clauses should ideally target unique identifiers).

### Creating Records (`create`)

The `create` action inserts a new record. The `data` block must contain all required fields not handled by defaults (like `@id`).

```json
{
  "model": "User",
  "action": "create",
  "data": {
    "name": "Alice",
    "age": 25
  },
  "select": { "__id": true, "name": true }
}
```

### Updating Records (`update`)

The `update` action modifies an existing record identified by the `where` block.

**Strict Target Verification**: Root-level `update` operations are strict. If the `where` block fails to match a record, the entire transaction will fail with a `Record not found` error.

**Technical Note**: The `where` block is parsed into a parameterized SQL `WHERE` clause. For security and predictability, it is highly recommended to target the primary key (`__id`) or a field marked `@unique`.

```json
{
  "model": "User",
  "action": "update",
  "where": { "__id": "user_id_123" },
  "data": {
    "age": 26
  },
  "select": { "__id": true, "age": true }
}
```

### Deleting Records (`delete`)

Removes a record from the database.

**Strict Target Verification**: Root-level `delete` operations are strict. If the `where` block fails to match a record, the entire transaction will fail with a `Record not found` error.

**Delete-Return Support**: You can use a `select` block with a `delete` action to retrieve data from the record immediately before it is removed. The engine fetches the record state within the same transaction to ensure consistency.

**Cascading Logic**: If the model has relationships with `onDelete: Cascade`, related records in other models will be deleted automatically by the database (or the engine for polymorphic bases).

```json
{
  "model": "User",
  "action": "delete",
  "where": { "__id": "user_id_123" },
  "select": { "__id": true, "name": true, "email": true }
}
```

### Upserting Records (`upsert`)

The `upsert` action performs an "Update or Create" operation.

**Expert Constraint**: The `where` block **MUST** target a field marked with `@id` or `@unique` in the schema. If the record exists, the `update` block is applied; otherwise, the `create` block is used to insert a new record.

```json
{
  "model": "User",
  "action": "upsert",
  "where": { "email": "alice@example.com" },
  "create": {
    "email": "alice@example.com",
    "name": "Alice"
  },
  "update": {
    "name": "Alice Updated"
  },
  "select": { "__id": true, "name": true }
}
```


## Nested Mutations

`caqui` allows you to perform operations on related models within the same transaction. Nested mutations are processed sequentially, and the engine automatically handles foreign key assignment.

### Scoped Nested Operations (Security)

Nested `update` and `delete` operations are **scoped to the parent relation**. 

**Technical Detail**: When you perform a nested update/delete, the engine automatically injects a constraint into the internal SQL query ensuring that the target child record is already linked to the parent record being mutated. This prevents "sideways" mutations where you might accidentally modify a record that belongs to a different relationship.

### Operational Availability

The available nested operations depend on the relationship cardinality and which model "owns" the foreign key (FK).

- **1:1 Relations**: If the parent model holds the FK, you can only `create`, `connect`, or `disconnect` a single record.
- **1:N Relations**: The "Many" side typically holds the FK. When mutating from the "One" side, you can perform bulk `create`, `update`, `delete`, and `upsert` operations.

### Relationship Management (`connect`, `disconnect`, `set`)

Instead of creating or deleting records, you can manage the links between existing records.

- **`connect`**: Links an existing record by its unique identifier.
- **`disconnect`**: Unlinks a record. For optional relations, this sets the FK to `NULL`. For required relations, this will trigger a validation error unless the record is re-linked in the same request.
- **`set`**: Replaces all current links with a new set of links. Internally, this performs a "Disconnect All" followed by multiple `connect` operations.

```json
{
  "model": "Post",
  "action": "update",
  "where": { "__id": "post_123" },
  "data": {
    "author": {
      "connect": { "__id": "author_456" }
    },
    "tags": {
      "set": [
        { "__id": "tag_1" },
        { "__id": "tag_2" }
      ]
    }
  }
}
```

### Nested `upsert` (Relation-Aware)

Nested upserts allow you to ensure a related record exists and is linked.

**Expert Detail**: The engine is "Relation-Aware." 
- For **Forward Relations** (parent holds the FK), it generates a UUID, inserts the child (if missing), and then updates the parent's FK.
- For **Reverse Relations** (child holds the FK), it uses the parent's ID as the conflict target on the child table, ensuring that each parent has exactly one related record in a 1:1 scenario.

```json
{
  "model": "User",
  "action": "update",
  "where": { "__id": "user_123" },
  "data": {
    "profile": {
      "upsert": {
        "create": { "bio": "New bio" },
        "update": { "bio": "Updated bio" }
      }
    }
  }
}
```

### Nested `create`, `update`, and `delete`

These follow the same logic as root-level actions but are applied to the related collection.

```json
{
  "model": "Author",
  "action": "create",
  "data": {
    "name": "Bob",
    "posts": {
      "create": [
        { "title": "Bob's First Post" },
        { "title": "Bob's Second Post" }
      ]
    }
  }
}
```


## Array Modifications

To append items to a `ScalarArray` or `EnumArray` directly, use the `push` operator.

**Expert Limitation**: The `push` operator is only available during `update` actions. When using `create`, the entire array must be provided as a literal list.

```json
{
  "model": "User",
  "action": "update",
  "where": { "__id": "user_123" },
  "data": {
    "roles": { "push": "EDITOR" }
  }
}
```

**Implementation Detail**: For SQLite, this uses `json_insert` to atomically append to the JSON array stored in the column.


## Polymorphic Mutations (Expert)

`caqui` supports mutations on polymorphic fields (both `Union` and `Base` types). The payload for a polymorphic field must explicitly specify the **target model name** to disambiguate the operation.

**Current Limitations**: Polymorphic fields currently support `connect`, `create`, and `disconnect` operations. Direct `update` or `delete` through a polymorphic field is not supported; these must be performed at the root level of the target model.

### Polymorphic `connect` and `create`

```json
{
  "model": "Activity",
  "action": "create",
  "data": {
    "title": "New Activity",
    "subject": {
      "User": {
        "connect": { "__id": "user_123" }
      }
    }
  }
}
```

In the example above, `subject` is a polymorphic field. By wrapping the `connect` in a `"User"` key, you tell the engine to link a record from the `User` model.

### Polymorphic `disconnect`

To remove a polymorphic link, use `disconnect: true`.

```json
{
  "model": "Activity",
  "action": "update",
  "where": { "__id": "act_456" },
  "data": {
    "subject": { "disconnect": true }
  }
}
```


## Validation & Normalization

The engine performs strict validation and normalization before generating SQL:

1.  **DateTime Normalization**: All `DateTime` values provided as ISO-8601 strings are normalized to UTC with millisecond precision (e.g., `2025-10-10T16:00:00.000Z`).
2.  **Enum Validation**: Values for Enum fields are checked against the allowed variants defined in the schema. Invalid variants trigger a `Validation Error`.
3.  **Type Safety**: `Float` and `Int` types are validated for numeric compatibility.


## Security & System Constraints

- **Transactional Integrity**: Every mutation request (including all nested operations) is wrapped in a single database transaction. If any step fails (e.g., a unique constraint violation), the entire transaction is rolled back.
- **Cascading Deletes (Bases)**: For polymorphic models extending a `Base`, the engine performs application-level cascading deletes to ensure that deleting a base record removes all referencing polymorphic links.
- **Read-Only Fields**: Fields marked as `@readonly` or internal tracking fields (like `updatedAt` managed by `@track`) cannot be manually updated via the API.
- **ID Immutability**: The primary identifier (`__id`) cannot be modified after a record is created.