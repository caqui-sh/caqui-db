# Schema DSL Reference

`caqui` uses a strict, declarative DSL to define your database schema and API structure in a single `schema.cq` file.

## Models

A `model` represents a database table and an entity in your API.

```
model User {
  id:   String @id @default(uuid())
  name: String
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
  id:    String   @id @default(uuid())
  tags:  String[]
}
```

## Relationships

Relationships link models together. `caqui` handles foreign key generation and deep relational integrity.

### 1:N (One-to-Many)

```
model Author {
  id:    String @id @default(uuid())
  posts: Post[]
}

model Post {
  id:     String @id @default(uuid())
  author: Author
}
```

### 1:1 (One-to-One)

```
model User {
  id:      String   @id @default(uuid())
  profile: Profile? @relation(fields: [profileId], references: [id])
  profileId: String? @unique
}

model Profile {
  id:   String @id @default(uuid())
  user: User?
}
```

### N:M (Many-to-Many)

N:M relationships require an explicit join table model.

```
model User {
  id:    String @id @default(uuid())
  roles: UserRole[]
}

model Role {
  id:    String @id @default(uuid())
  users: UserRole[]
}

model UserRole {
  userId: String
  roleId: String
  user:   User @relation(fields: [userId], references: [id])
  role:   Role @relation(fields: [roleId], references: [id])
  @@id([userId, roleId])
}
```

## Polymorphic Unions

Unions allow a single property to return different models dynamically.

```
union SearchResult = Post | Author

model SearchQuery {
  id:      String       @id @default(uuid())
  results: SearchResult
}
```

## Field Attributes

Attributes customize the behavior of individual fields.

| Attribute | Description |
| :--- | :--- |
| `@id` | Marks the field as the Primary Key. **Must not be optional (`?`).** |
| `@unique` | Ensures all values in the column are unique. |
| `@default(...)` | Sets a default value (e.g., `uuid()`, `autoincrement()`, `now()`, or static values like `"string"` or `42`). |
| `@updatedAt` | Automatically updates the timestamp on modification. |
| `@map("name")` | Maps the field to a different underlying database column name. |
| `@ignore` | Prevents the field from being read or written via the API. |
| `@relation(...)` | Defines the relationship. Accepts `fields` (local keys), `references` (foreign keys), `onDelete` (e.g., `CASCADE`, `SET NULL`, `RESTRICT`, `NO ACTION`), and `deferrable` (for deferring constraint checks). |

### Functions in `@default`

- `uuid()`: Generates a sequential UUIDv7.
- `autoincrement()`: Automatically incrementing IDs.
- `now()`: The current UTC timestamp.
- `"static"`: A static string, number, or boolean default.

## Block Attributes

Attributes that apply to the entire model.

| Attribute | Description |
| :--- | :--- |
| `@@unique([f1, f2])` | Defines a composite unique constraint. |
| `@@index([f1, f2])` | Defines a composite database index. |
| `@@id([f1, f2])` | Defines a composite primary key. |