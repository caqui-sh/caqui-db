# Schema DSL Reference

`caqui` uses a strict, declarative DSL to define your database schema and API structure in a single `schema.cq` file. The engine parses this file at runtime to dynamically mount the API and synchronize the database state.

## Models

A `model` represents a database table and a primary entity in your API. Models inherit a set of automatic system fields (like `__id` and `__kind`).

```prisma
model User {
  name: String
  @@id(uuid)
}
```

## Scalar Types

The following scalar types are supported. All scalars map directly to SQLite affinity types but are strictly validated at the API layer.

| Type | Description | SQLite Affinity |
| :--- | :--- | :--- |
| `String` | UTF-8 encoded string. | TEXT |
| `Int` | 64-bit signed integer. | INTEGER |
| `Float` | 64-bit floating point number. | REAL |
| `Boolean` | Stored as 0 or 1. | INTEGER |
| `DateTime` | ISO 8601 formatted string (normalized to UTC). | TEXT |

### Type Modifiers

Fields can be modified to change their cardinality or nullability:

- **Optional (`?`)**: Allows the field to be `null`. Example: `String?`
- **Array (`[]`)**: Native primitive array storage. Arrays are always non-optional (they return an empty list if empty). Example: `String[]`

## Enums

Enums define a fixed set of possible values, providing strict type safety for categorical data.

```prisma
enum Role {
  ADMIN
  USER
  GUEST
}

model User {
  role: Role
  roles: Role[] // Enum arrays are supported
  @@id(uuid)
}
```

## Relationships

Relationships link models together. `caqui` handles foreign key generation and deep relational integrity automatically.

### Implicit Relations (Idiomatic)

In most cases, you can omit the `@relation` attribute. `caqui` will automatically detect the relationship and inject the appropriate foreign keys (e.g., `authorId`) and constraints.

```prisma
model Author {
  posts: Post[]
  @@id(uuid)
}

model Post {
  title:  String
  author: Author
  @@id(uuid)
}
```

### Explicit Relations & Disambiguation

Use the `@relation` attribute to override default field names or to disambiguate multiple relations pointing to the same model.

#### Disambiguation (Named Relations)

When a model has multiple fields pointing to the same target model, you **must** provide a unique name to each relation.

```prisma
model User {
  authoredPosts: Post[] @relation("Author")
  reviewedPosts: Post[] @relation("Reviewer")
  @@id(uuid)
}

model Post {
  title:    String
  author:   User   @relation("Author")
  reviewer: User   @relation("Reviewer")
  @@id(uuid)
}
```

#### Explicit Key Mapping

You can manually specify which local fields and remote references should be used for the relationship.

```prisma
model Profile {
  user: User @relation(fields: [userId], references: [__id])
  userId: String @unique
  @@id(uuid)
}
```

### Referential Integrity (`onDelete`)

Control how the system behaves when a related record is deleted.

| Option | Behavior |
| :--- | :--- |
| `CASCADE` | Deletes the child records when the parent is deleted. |
| `SET NULL` | Sets the foreign key to null (requires an optional field). |
| `RESTRICT` | Prevents deletion of the parent if child records exist. |
| `NO ACTION` | (Default) Similar to restrict but verified at the transaction end. |

```prisma
model Post {
  author: User @relation(onDelete: CASCADE)
  @@id(uuid)
}
```

## Polymorphism

`caqui` provides two powerful patterns for managing heterogeneous data: Unions and Abstract Bases.

### Polymorphic Unions

Unions allow a single property to return different models dynamically. Unlike inheritance, models in a union do not need to share any common fields.

```prisma
union SearchResult = Post | Author

model SearchQuery {
  results: SearchResult[]
  @@id(uuid)
}
```

### Polymorphic Bases (Inheritance)

Abstract Bases allow you to define common fields that can be inherited by multiple models. When a model `extends` a base, it gains all of its fields.

**Technical Note**: Bases are abstract and cannot be instantiated directly. They are used to group related models for polymorphic queries and to share schema definitions.

#### Inheritance & Trait Flattening

A model can extend multiple bases, resulting in "Transitive Trait Flattening" where fields are merged into the final model.

```prisma
base Node {
  id: String @unique
}

base Timestamped extends Node {
  createdAt: DateTime
}

model User extends Timestamped {
  name: String
  @@id(uuid)
}
```

In the example above, `User` will effectively have `id`, `createdAt`, and `name` fields.

#### Polymorphic Base Relations

You can create relationships that point to an abstract `base`. This allows you to link to any record that implements that base.

```prisma
base Content {
  title: String
}

model Article extends Content {
  body: String
  @@id(uuid)
}

model Video extends Content {
  url: String
  @@id(uuid)
}

model Collection {
  items: Content[] // Can contain both Articles and Videos
  @@id(uuid)
}
```

## Field Attributes

Attributes customize the behavior of individual fields.

| Attribute | Description |
| :--- | :--- |
| `@id` | **(Internal)** Marks a field as the primary key. Manual use is restricted as `__id` is auto-generated. |
| `@unique` | Ensures all values in the column are unique. Supports optional fields (allows multiple NULLs). |
| `@track` | Automatically injects a hidden `__<FieldName>_updatedAt` timestamp field that updates only when this specific field is modified. |
| `@relation(...)` | Configures relationships. Parameters: `name`, `fields`, `references`, `onDelete`. |

## Block Attributes

Attributes that apply to the entire model.

| Attribute | Description |
| :--- | :--- |
| `@@id(strategy)` | Configures the primary key generation strategy for the automatic `__id` field. Options: `uuid`, `cuid`, `autoincrement` (default). |
| `@@track` | Automatically injects a hidden `__updatedAt` timestamp field into the model that updates anytime the record is modified. |
| `@@fulltext([fields])` | Creates a shadow Full-Text Search (FTS5) table for high-performance string matching across the specified fields. |

```prisma
model Document {
  title: String
  body:  String
  @@fulltext([title, body])
  @@id(uuid)
}
```


## Expert: Engine Internals

Understanding how `caqui` manages your data at the database level is critical for expert-level system design.

### Reserved Synthetic Fields

The engine automatically injects several reserved fields into every model during the validation and crystallization phase. These fields are accessible via the API but cannot be manually defined in the DSL.

| Field Name | Type | Description |
| :--- | :--- | :--- |
| `__id` | `String` / `Int` | The primary identifier. Its type is determined by the `@@id` strategy. Defaults to `Int` (autoincrement). |
| `__kind` | `String` | Returns the concrete model name (e.g., `"User"`). Used for client-side type narrowing. |
| `__<BaseName>` | `Boolean` | (Polymorphic Only) Injected for every base the model implements. Returns `true` if the record implements that base. |
| `__updatedAt` | `DateTime` | (Tracking Only) Injected when `@@track` is present on the model. |
| `__<FieldName>_updatedAt` | `DateTime` | (Tracking Only) Injected for every field marked with `@track`. |

### Inheritance Shadowing Rules

When a model `extends` a base, it may redefine a field already present in the base (shadowing). However, `caqui` enforces a **Strict Signature Match**: the shadowed field must have the exact same type, optionality, and attributes as the base definition. Mismatched signatures will trigger a validation error during the crystallization phase.

### Relationship Injection

`caqui` follows a "Strict Foreign Key" policy. For every relationship defined in the DSL, the engine ensures a physical foreign key column exists. If you do not provide one (the idiomatic path), the engine injects a hidden `[fieldName]Id` column automatically.

### Comments

The `schema.cq` DSL supports single-line comments using the double-slash syntax.

```prisma
// This is a comment
model User {
  name: String // Field comment
  @@id(uuid)
}
```
