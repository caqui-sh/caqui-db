# API Query Reference (Read)

`caqui` exposes a single, universal HTTP POST endpoint at `/api/v1/query`. This endpoint is used for all data retrieval operations.

Instead of traditional REST routes, you send a JSON payload describing the data graph you want to fetch.

## The `findMany` Action

To fetch one or more records, use the `findMany` action.

```bash
curl -X POST http://localhost:4000/api/v1/query \
  -H "Content-Type: application/json" \
  -d '{
    "model": "User",
    "action": "findMany",
    "select": {
        "id": true,
        "name": true
    }
  }'
```

## Field Projections (`select`)

The `select` block allows you to specify exactly which fields you want returned. This prevents over-fetching and reduces payload size.

```json
{
  "model": "User",
  "action": "findMany",
  "select": {
    "id": true,
    "email": true,
    "profile": {
      "select": {
        "bio": true
      }
    }
  }
}
```

## Filtering (`where`)

Use the `where` block to filter your results.

### Simple Operators

- `eq`: Equal
- `notEq`: Not Equal
- `gt`: Greater Than
- `gte`: Greater Than or Equal
- `lt`: Less Than
- `lte`: Less Than or Equal
- `in`: Included in array of values

```json
{
  "where": {
    "age": { "gte": 18 },
    "status": { "in": ["ACTIVE", "PENDING"] }
  }
}
```

### Nullability Operators

- `IsNull`: Field is null
- `IsNotNull`: Field is not null

```json
{
  "where": {
    "deletedAt": "IsNull"
  }
}
```

### Logical Operators (`AND`, `OR`)

Combine multiple conditions.

```json
{
  "where": {
    "OR": [
      { "role": { "eq": "ADMIN" } },
      { "isSuperuser": { "eq": true } }
    ]
  }
}
```

## Relational Filtering

`caqui` supports deep filtering based on the properties of related models.

### For To-Many Relations (`some`, `every`, `none`)

- `some`: Returns records where at least one related record matches.
- `every`: Returns records where all related records match (or none exist).
- `none`: Returns records where no related records match.

```json
// Find users who have at least one post titled "Hello"
{
  "model": "User",
  "where": {
    "posts": {
      "some": {
        "title": { "eq": "Hello" }
      }
    }
  }
}
```

### For To-One Relations (`is`, `isNot`)

- `is`: Returns records where the related record matches.
- `isNot`: Returns records where the related record does not match.

```json
// Find users whose profile bio is "Hi"
{
  "model": "User",
  "where": {
    "profile": {
      "is": {
        "bio": { "eq": "Hi" }
      }
    }
  }
}
```

## Querying Polymorphic Unions

When a field is defined as a `union` in your schema, you must specify which fields to return for each possible model using **target fragments**.

```json
{
  "model": "SearchQuery",
  "action": "findMany",
  "select": {
    "id": true,
    "content": {
      "Post": { 
        "select": { "title": true } 
      },
      "User": { 
        "select": { "name": true } 
      }
    }
  }
}
```

If the `content` of a record is a `Post`, the API will return a `title`. If it is a `User`, it will return a `name`.

## Pagination

Use `limit` and `offset` to paginate through large datasets.

```json
{
  "model": "Post",
  "action": "findMany",
  "limit": 10,
  "offset": 20,
  "select": { "title": true }
}
```
