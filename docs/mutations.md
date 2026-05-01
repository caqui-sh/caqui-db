# API Mutation Reference (Write)

`caqui` provides a powerful, transaction-safe mutation engine that allows you to perform complex, nested write operations in a single request.

All mutations are performed via the same `/api/v1/query` endpoint.

## Root-Level Actions

### Creating Records (`create`)

Use the `create` action to insert new records.

```json
{
  "model": "User",
  "action": "create",
  "data": {
    "name": "Alice",
    "age": 25
  },
  "select": { "id": true, "name": true }
}
```

### Updating Records (`update`)

Use the `update` action to modify existing records. You must provide a `where` block to identify the target record.

```json
{
  "model": "User",
  "action": "update",
  "where": { "id": "user_id_123" },
  "data": {
    "age": 26
  },
  "select": { "id": true, "age": true }
}
```

### Deleting Records (`delete`)

Use the `delete` action to remove records. You must provide a `where` block to identify the record.

```json
{
  "model": "User",
  "action": "delete",
  "where": { "id": "user_id_123" },
  "select": { "id": true }
}
```

### Upserting Records (`upsert`)

The `upsert` action allows you to update an existing record or create a new one if it doesn't exist. The `where` block MUST target a unique identifier (like an `@id` or `@unique` field). You must provide both a `create` block and an `update` block.

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
  "select": { "id": true, "name": true }
}
```

## Nested Mutations

`caqui` allows you to perform operations on related models within the same transaction.

### Nested `create`

You can create related records inline.

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
  },
  "select": { "id": true }
}
```

### Nested `update`

You can update related records inline. You must provide a `where` block to identify the related record and a `data` block with the modifications.

```json
{
  "model": "Author",
  "action": "update",
  "where": { "id": "author_123" },
  "data": {
    "posts": {
      "update": [
        {
          "where": { "id": "post_456" },
          "data": { "title": "New Title" }
        }
      ]
    }
  },
  "select": { "id": true }
}
```

### Nested `delete`

You can delete related records inline using a `where` block.

```json
{
  "model": "Author",
  "action": "update",
  "where": { "id": "author_123" },
  "data": {
    "posts": {
      "delete": [
        { "id": "post_789" }
      ]
    }
  },
  "select": { "id": true }
}
```

### Nested `upsert`

You can perform upsert operations on related records inline. Like the root-level `upsert`, it requires a `where`, `create`, and `update` block for each record.

```json
{
  "model": "Author",
  "action": "update",
  "where": { "id": "author_123" },
  "data": {
    "posts": {
      "upsert": [
        {
          "where": { "id": "post_101" },
          "create": { "title": "A Brand New Post" },
          "update": { "title": "An Updated Post" }
        }
      ]
    }
  },
  "select": { "id": true }
}
```

## Managing Relationship Links

Instead of creating or deleting related records, you can safely link or unlink existing ones using their unique identifiers.

- `connect`: Link an existing record by its unique identifier.
- `disconnect`: Unlink a record (sets the foreign key to NULL or removes it from the join).
- `set`: Replaces all current links with a new set of links.

```json
{
  "model": "Post",
  "action": "update",
  "where": { "id": "post_123" },
  "data": {
    "author": {
      "connect": { "id": "author_456" }
    },
    "tags": {
      "set": [
        { "id": "tag_1" },
        { "id": "tag_2" }
      ]
    }
  },
  "select": { "id": true }
}
```

## Array Modifications

To append items to a `ScalarArray` directly, use the `push` operator.

```json
{
  "model": "User",
  "action": "update",
  "where": { "id": "user_123" },
  "data": {
    "roles": { "push": "EDITOR" }
  },
  "select": { "id": true }
}
```

## Security & Constraints

- **Transactional Integrity**: If any part of a nested mutation fails (e.g., a foreign key violation in a child record), the entire transaction is rolled back.
- **`@ignore` enforcement**: Fields marked with `@ignore` in the schema cannot be written to via the API.
- **Unique Enforcement**: `caqui` checks for unique constraint violations before attempting database writes to provide clear error messages.