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

`caqui` supports the following simple operators. Note that text search operators like `contains`, `startsWith`, and `endsWith` are NOT supported.

- `eq`: Equal
- `notEq`: Not Equal
- `gt`: Greater Than
- `gte`: Greater Than or Equal
- `lt`: Less Than
- `lte`: Less Than or Equal
- `in`: Included in an array of values

```json
{
  "model": "User",
  "action": "findMany",
  "where": {
    "age": { "gte": 18, "lt": 65 },
    "status": { "in": ["ACTIVE", "PENDING"] },
    "role": { "notEq": "GUEST" }
  },
  "select": { "id": true }
}
```

### Logical Operators (`AND`, `OR`)

Combine multiple conditions using `AND` and `OR`.

```json
{
  "model": "User",
  "action": "findMany",
  "where": {
    "OR": [
      { "role": { "eq": "ADMIN" } },
      { 
        "AND": [
          { "isSuperuser": { "eq": true } },
          { "status": { "eq": "ACTIVE" } }
        ]
      }
    ]
  },
  "select": { "id": true }
}
```

### Nullability Operators

- `IsNull`: Field is null
- `IsNotNull`: Field is not null

```json
{
  "model": "User",
  "action": "findMany",
  "where": {
    "deletedAt": "IsNull"
  },
  "select": { "id": true }
}
```

## Relational Filtering

`caqui` supports deep filtering based on the properties of related models.

### For To-Many Relations (`some`, `every`, `none`)

- `some`: Returns records where at least one related record matches the condition.
- `every`: Returns records where all related records match the condition (or none exist).
- `none`: Returns records where no related records match the condition.

```json
{
  "model": "User",
  "action": "findMany",
  "where": {
    "posts": {
      "some": {
        "published": { "eq": true }
      }
    }
  },
  "select": { "id": true }
}
```

### For To-One Relations (`is`, `isNot`)

- `is`: Returns records where the related record matches the condition.
- `isNot`: Returns records where the related record does not match the condition.

```json
{
  "model": "User",
  "action": "findMany",
  "where": {
    "profile": {
      "is": {
        "bio": { "IsNotNull": true }
      }
    }
  },
  "select": { "id": true }
}
```

## Querying Polymorphic Unions and Union Arrays

When a field is defined as a `union` or a union array in your schema, you must specify which fields to return for each possible model using **fragment selection blocks** directly inside the union field object.

```json
{
  "model": "SearchQuery",
  "action": "findMany",
  "select": {
    "id": true,
    "results": {
      "Post": { 
        "select": { "title": true } 
      },
      "Author": { 
        "select": { "name": true } 
      }
    }
  }
}
```

If the `results` of a record (or items in the array) resolve to a `Post`, the API will return a `title`. If they resolve to an `Author`, it will return a `name`.

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