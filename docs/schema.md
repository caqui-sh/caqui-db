# Schema DSL Reference

`caqui` uses a strict, declarative DSL to define your database schema and API structure in a single `schema.cq` file.

## Models

A `model` represents a table in your SQLite database and an entity in your API.

```graphql
model User {
  id:   String @id @default(uuid())
  name: String
}
```

## Primitive Types

The following scalar types are supported:

| Type | SQLite Type | Description |
| :--- | :--- | :--- |
| `String` | `TEXT` | UTF-8 encoded string. |
| `Int` | `INTEGER` | 64-bit signed integer. |
| `Float` | `REAL` | 64-bit floating point number. |
| `Boolean` | `INTEGER` | Stored as 0 or 1. |
| `DateTime` | `TEXT` | ISO 8601 formatted string. |

## Arrays

`caqui` supports native storage of primitive arrays 

```graphql
model Post {
  id:    String   @id @default(uuid())
  tags:  String[]
}
```

## Relationships

Relationships link models together. `caqui` handles foreign key generation and deep relational integrity.

### 1:N (One-to-Many)

```graphql
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

```graphql
model User {
  id:      String   @id @default(uuid())
  profile: Profile? @relation(column: "profileId")
}

model Profile {
  id:   String @id @default(uuid())
  user: User?
}
```

### N:M (Many-to-Many)

N:M relationships currently require an explicit join table model.

```graphql
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

```graphql
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
| `@id` | Marks the field as the Primary Key. |
| `@unique` | Ensures all values in the column are unique. |
| `@default(...)` | Sets a default value (see [Functions](#functions)). |
| `@updatedAt` | Automatically updates the timestamp on modification. |
| `@map("name")` | Maps the field to a different database column name. |
| `@ignore` | Prevents the field from being read or written via the API. |
| `@relation(...)` | Defines the fields and references for a relationship. |

### Functions in `@default`

- `uuid()`: Generates a sequential UUIDv7.
- `autoincrement()`: automatically incrementing IDs.
- `now()`: The current UTC timestamp.
- `"static"`: A static string, number, or boolean default.

## Block Attributes

Attributes that apply to the entire model.

| Attribute | Description |
| :--- | :--- |
| `@@unique([f1, f2])` | Defines a composite unique constraint. |
| `@@index([f1, f2])` | Defines a composite database index. |
| `@@id([f1, f2])` | Defines a composite primary key. |
