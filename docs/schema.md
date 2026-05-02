# Schema DSL Reference

`caqui` uses a strict, declarative DSL to define your database schema and API structure in a single `schema.cq` file.

## Models

A `model` represents a database table and an entity in your API. All models possess an automatic primary key field named `__id`.

```
model User {
  name: String
  @@id(uuid)
}
```

## Scalar Types

The following scalar types are supported:

| Type | Description |
| :--- | :--- |
| `String` | UTF-8 encoded string. |
| `Int` | 64-bit signed integer. |
| `Float` | 64-bit floating point number. |
| `Boolean` | Stored as true or false. |
| `DateTime` | ISO 8601 formatted string. |

### Type Modifiers

Scalar and model types can be modified to change their cardinality or nullability:

- **Optional (`?`)**: Appending `?` makes a field optional (nullable). Example: `String?`
- **Array (`[]`)**: Appending `[]` makes a field an array (list) of that type. Example: `String[]`

## Arrays

`caqui` supports native storage of primitive arrays.

```
model Post {
  tags:  String[]
  @@id(uuid)
}
```

## Relationships

Relationships link models together. `caqui` handles foreign key generation and deep relational integrity.

### 1:N (One-to-Many)

```
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

### 1:1 (One-to-One)

```
model User {
  profile: Profile? @relation(fields: [profileId], references: [__id])
  profileId: String? @unique
  @@id(uuid)
}

model Profile {
  user: User?
  @@id(uuid)
}
```

### N:M (Many-to-Many)

N:M relationships require an explicit join table model.

```
model User {
  roles: UserRole[]
  @@id(uuid)
}

model Role {
  users: UserRole[]
  @@id(uuid)
}

model UserRole {
  userId: String
  roleId: String
  user:   User @relation(fields: [userId], references: [__id])
  role:   Role @relation(fields: [roleId], references: [__id])
  @@id(uuid)
}
```

## Polymorphic Unions

Unions allow a single property to return different models dynamically.

```
union SearchResult = Post | Author

model SearchQuery {
  results: SearchResult
  @@id(uuid)
}
```

## Field Attributes

Attributes customize the behavior of individual fields.

| Attribute | Description |
| :--- | :--- |
| `@unique` | Ensures all values in the column are unique. |
| `@track` | Automatically injects a sibling hidden `__<FieldName>_updatedAt` timestamp field that only updates when this specific field is modified. |
| `@map("name")` | Maps the field to a different underlying database column name. |
| `@relation(...)` | Defines the relationship. Accepts `fields` (local keys), `references` (foreign keys), `onDelete` (e.g., `CASCADE`, `SET NULL`, `RESTRICT`, `NO ACTION`), and `deferrable` (for deferring constraint checks). |

## Block Attributes

Attributes that apply to the entire model.

| Attribute | Description |
| :--- | :--- |
| `@@unique([f1, f2])` | Defines a composite unique constraint. |
| `@@index([f1, f2])` | Defines a composite database index. |
| `@@id(strategy)` | Defines the primary key strategy (`uuid`, `cuid`, or `autoincrement`). |
| `@@track` | Automatically injects a hidden `__updatedAt` timestamp field into the model that updates anytime the record is modified. |
