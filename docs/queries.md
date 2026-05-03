# API Query Reference (Read)

`caqui` uses a "Zero-Overhead" query engine designed for high-performance graph traversal. Unlike traditional REST or GraphQL APIs that suffer from N+1 query problems, `caqui` compiles your entire request—no matter how deeply nested—into a **single, highly optimized SQL query**.

All queries are performed via the same `/api/v1/query` endpoint using the `POST` method.

## Root-Level Actions

### Fetching Records (`findMany`)

The `findMany` action is the primary workhorse for all data retrieval. It can fetch a single record (by PK) or a collection of records.

```json
{
  "model": "User",
  "action": "findMany",
  "select": {
    "__id": true,
    "name": true
  }
}
```

## Projections (`select`)

The `select` block defines exactly which fields and relations to return. 

### Nested Relational Selects

You can fetch related models by nesting a `select` block within a relation field. The engine fetches these in the same transaction as the parent, ensuring perfect consistency.

```json
{
  "model": "User",
  "action": "findMany",
  "select": {
    "__id": true,
    "email": true,
    "posts": {
      "select": {
        "title": true,
        "content": true
      }
    }
  }
}
```

### Implicit Fields

- `__id`: The globally unique identifier for the record.
- `__kind`: Available on polymorphic reads (Unions/Bases) to identify the concrete model of the returned record.


## Filtering (`where`)

Use the `where` block to filter your results. Filters can be applied to scalar fields and relations.

### Scalar Operators

| Operator | Description | Supported Types |
| :--- | :--- | :--- |
| `eq` / `notEq` | Equality / Inequality | All (including Enums) |
| `gt` / `gte` | Greater Than (or Equal) | Int, Float, DateTime |
| `lt` / `lte` | Less Than (or Equal) | Int, Float, DateTime |
| `in` | Included in a list | All |
| `contains` | Substring search | String |
| `startsWith` | Prefix search | String |
| `endsWith` | Suffix search | String |
| `isNull` | Check for NULL (expects boolean) | All |

```json
{
  "model": "User",
  "action": "findMany",
  "where": {
    "age": { "gte": 18 },
    "name": { "startsWith": "A" },
    "deletedAt": { "isNull": true }
  }
}
```

### Logical Operators (`AND`, `OR`)

Conditions in the `where` block are joined by `AND` by default. You can explicitly use `AND` and `OR` for complex logic.

```json
{
  "model": "User",
  "where": {
    "OR": [
      { "role": "ADMIN" },
      { "AND": [{ "age": { "gte": 21 } }, { "status": "ACTIVE" }] }
    ]
  }
}
```

### Relational Filtering

`caqui` supports deep filtering based on properties of related records.

#### For List Relations (`some`, `every`, `none`)

- **`some`**: At least one related record matches.
- **`every`**: All related records match (or none exist).
- **`none`**: No related records match.

```json
{
  "model": "User",
  "where": {
    "posts": {
      "some": { "published": true }
    }
  }
}
```

#### For Singular Relations (`is`, `isNot`)

- **`is`**: The related record matches.
- **`isNot`**: The related record does not match (or is null).

```json
{
  "model": "User",
  "where": {
    "profile": {
      "is": { "bio": { "IsNotNull": true } }
    }
  }
}
```

### Polymorphic Filtering

When filtering a polymorphic field (`Union` or `Base`), you can target a specific model by wrapping the filter in the model's name.

```json
{
  "model": "Activity",
  "where": {
    "subject": {
      "User": { "name": { "contains": "Alice" } }
    }
  }
}
```

### Expert: Type Discrimination Markers

On polymorphic bases, you can select or filter by synthetic markers prefixed with `__` to identify or narrow types.

- **Selection**: Include `__ModelName: true` in your `select` block to receive a boolean field indicating if the record is of that type.
- **Filtering**: Use `__ModelName: true` in your `where` block to filter the result set to specific concrete implementations.

```json
{
  "model": "ContentBase",
  "where": { "__Article": true },
  "select": { "title": true }
}
```


## Polymorphic Queries (Unions & Bases)

When a field is defined as a `Union` or a `Base`, you must use **fragment selection blocks** to specify which fields to return for each concrete model.

### Fragment Selection

```json
{
  "model": "Activity",
  "action": "findMany",
  "select": {
    "__id": true,
    "subject": {
      "User": { "select": { "name": true } },
      "Team": { "select": { "handle": true } }
    }
  }
}
```

The returned payload will include a `__kind` field for each polymorphic record, allowing client-side type narrowing.

### Querying Abstract Bases

You can query a `Base` directly as the root model. The engine will perform a high-performance `UNION ALL` across all implementing models.

```json
{
  "model": "ContentBase",
  "action": "findMany",
  "select": {
    "__id": true,
    "__kind": true,
    "title": true
  }
}
```


## Global Search (`search`)

The top-level `search` parameter performs a Full-Text Search (FTS) across all indexed columns of the root model. 

```json
{
  "model": "Post",
  "action": "findMany",
  "search": "caqui OR database",
  "select": { "title": true }
}
```

**Expert Tip: `search` vs. `contains`**:
- **`search`**: Uses Full-Text Search (FTS5). It is extremely fast ($O(\log N)$) but only works on full words or prefix queries.
- **`contains`**: Uses SQL `LIKE %...%`. It is slower ($O(N)$ full-scan) but can match any substring regardless of word boundaries.


## Ordering and Pagination

`caqui` supports robust sorting and offset-based pagination at both the root level and within nested relations.

### Sorting (`orderBy`)

The `orderBy` block accepts an object where keys are field names and values are `asc` or `desc`.

```json
{
  "model": "User",
  "orderBy": {
    "createdAt": "desc",
    "name": "asc"
  },
  "select": { "__id": true }
}
```

### Pagination (`limit` and `skip`)

- **`limit`**: The maximum number of records to return.
- **`skip`**: The number of records to skip (offset).

```json
{
  "model": "Post",
  "limit": 10,
  "skip": 20,
  "select": { "title": true }
}
```

### Expert: Nested Pagination & Ordering

Unlike many APIs, `caqui` allows you to sort and paginate **inside related collections**.

```json
{
  "model": "Author",
  "select": {
    "name": true,
    "posts": {
      "select": { "title": true },
      "limit": 5,
      "orderBy": { "createdAt": "desc" }
    }
  }
}
```

**How it works**: The engine utilizes SQL Window Functions (`ROW_NUMBER() OVER (PARTITION BY ...)`) to perform efficient, per-parent pagination in a single SQL query.


## Technical Internals & Constraints

### Data Normalization
The engine performs automatic normalization to ensure query consistency:
- **DateTime**: All input strings in `where` filters are normalized to UTC with millisecond precision before the SQL is generated.
- **Booleans**: Since SQLite lacks a native boolean type, `caqui` maps `0/1` integers to JSON `false/true` transparently in the projection layer.

### N+1 Neutralization
`caqui` achieves its "Zero-Overhead" status by collapsing the entire query graph into a single SQL statement. It uses:
1. **Correlated Subqueries**: To traverse relationships.
2. **JSON Aggregation**: `json_group_array` and `json_object` to build the JSON response directly within SQLite's C-layer.

### Security Limits
- **Maximum Query Depth**: By default, queries are limited to a depth of **10 levels** to prevent recursive complexity attacks.
- **Strict Schema Enforcement**: Requests for fields not defined in the `schema.cq` are rejected at the API layer with a 400 Bad Request.