# Advanced Relational Rules

`caqui` supports complex data modeling requirements, including ambiguous relationships, self-referential hierarchies, and fine-grained referential integrity.

## Named Relations (Disambiguation)

When you have multiple relationships between the same two models, you must name them to tell `caqui` which fields belong to which relation.

```prisma
model User {
  id:            String @id
  authoredPosts: Post[] @relation("AuthorToPost")
  reviewedPosts: Post[] @relation("ReviewerToPost")
}

model Post {
  id:         String @id
  authorId:   String
  author:     User   @relation("AuthorToPost", fields: [authorId], references: [id])
  reviewerId: String
  reviewer:   User   @relation("ReviewerToPost", fields: [reviewerId], references: [id])
}
```

## Self-Referential Relations

You can model hierarchies where a model points back to itself.

```prisma
model Employee {
  id:              String     @id
  name:            String
  managerId:       String?
  manager:         Employee?  @relation("Management", fields: [managerId], references: [id])
  directReports:   Employee[] @relation("Management")
}
```

## Cascading Deletes (`onDelete`)

You can control what happens to related records when a parent record is deleted using the `onDelete` attribute in the `@relation`.

| Option | Behavior |
| :--- | :--- |
| `Cascade` | Automatically deletes child records when the parent is deleted. |
| `SetNull` | Sets the foreign key in child records to `NULL` (requires the field to be optional). |
| `Restrict` | Prevents the parent from being deleted if child records exist. |

```prisma
model User {
  id:    String @id
  posts: Post[]
}

model Post {
  id:       String @id
  userId:   String
  user:     User   @relation(fields: [userId], references: [id], onDelete: Cascade)
}
```

## Deferrable Constraints

By default, SQLite enforces foreign key constraints immediately. However, complex operations (like cyclic inserts or swapping records) can temporarily violate these constraints.

Using `deferrable: true` tells `caqui` to wait until the end of a transaction to verify the constraint.

```prisma
model User {
  id:        String  @id
  profileId: String
  profile:   Profile @relation(fields: [profileId], references: [id], deferrable: true)
}

model Profile {
  id:     String @id
  userId: String
  user:   User   @relation(fields: [userId], references: [id], deferrable: true)
}
```

This allows you to perform "circular" inserts:
1. `BEGIN TRANSACTION;`
2. Insert `User` (pointing to a profile that doesn't exist yet).
3. Insert `Profile` (pointing to the user).
4. `COMMIT;` (The engine verifies both exist now).
