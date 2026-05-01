# API Mutation Reference (Write)

`caqui` provides a powerful, transaction-safe mutation engine that allows you to perform complex, nested write operations in a single request.

All mutations are performed via the same `/api/v1/query` endpoint.

## Creating Records (`create`)

Use the `create` action to insert new records.

```json
{
  "model": "User",
  "action": "create",
  "data": {
    "name": "Alice",
    "age": 25
  },
  "select": { "id": true }
}
```

## Updating Records (`update`)

Use the `update` action to modify existing records. You must provide a `where` block to identify the target record.

```json
{
  "model": "User",
  "action": "update",
  "where": { "id": "user_id_123" },
  "data": {
    "age": 26
  },
  "select": { "age": true }
}
```

## Deleting Records (`delete`)

Use the `delete` action to remove records.

```json
{
  "model": "User",
  "action": "delete",
  "where": { "id": "user_id_123" },
  "select": { "id": true }
}
```

## Upserting Records (`upsert`)

The `upsert` action allows you to update an existing record or create a new one if it doesn't exist. The `where` block MUST target an `@id` or `@unique` field.

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
  "select": { "id": true }
}
```

## Nested Mutations

`caqui` allows you to perform operations on related models within the same transaction.

### Nested `create`

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

### Nested `update` & `delete`

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
      ],
      "delete": [
        { "id": "post_789" }
      ]
    }
  }
}
```

## Managing Relationship Links

Instead of creating or deleting related records, you can safely link or unlink existing ones.

- `connect`: Link an existing record by its unique identifier.
- `disconnect`: Unlink a record (sets foreign key to NULL).
- `set`: Replaces all current links with a new set of links.

```json
{
  "model": "Post",
  "action": "update",
  "where": { "id": "post_123" },
  "data": {
    "author": {
      "connect": { "id": "author_456" }
    }
  }
}
```

## Array Modifications

To append items to a `ScalarArray` (JSON column), use the `push` operator.

```json
{
  "model": "User",
  "action": "update",
  "where": { "id": "user_123" },
  "data": {
    "tags": { "push": "new-tag" }
  }
}
```

## Security & Constraints

- **Transactional Integrity**: If any part of a nested mutation fails (e.g., a foreign key violation in a child record), the entire transaction is rolled back.
- **`@ignore` enforcement**: Fields marked with `@ignore` in the schema cannot be written to via the API.
- **Unique Enforcement**: `caqui` checks for unique constraint violations before attempting database writes to provide clear error messages.
