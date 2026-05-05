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

---

## Abstract Base Shapes & Polymorphism

Caqui features a sophisticated polymorphic execution engine that treats abstract base shapes differently depending on the context of the mutation.

### Singular Mutations on Base Shapes
When performing singular actions (`create`, `update`, `delete`, `connect`, `disconnect`), you **cannot** mutate an abstract base shape directly. The engine actively prevents you from doing so.

To maintain optimal execution speed and prevent the engine from having to "guess" the target table, you must explicitly disambiguate which concrete table you are targeting.

**Example: Updating a Polymorphic Field**
Use the `__kind` attribute in your payload to explicitly resolve the target:
```json
{
  "update": {
    "__kind": "Article",
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

## Arrays and Relational Limitations

While the nested mutation system is highly flexible, there are a few important limitations to keep in mind regarding SQLite and relational structure:

1. **Foreign-Key Array Creation:**
   If your parent model physically holds the foreign key for a relation, you cannot perform a nested array `create`. A single foreign key column cannot point to multiple newly created child rows.
