# Error Handling & Diagnostics

`caqui` communicates failures through distinct channels depending on the context: CLI output for startup and tooling errors, and standard HTTP status codes with plain-text messages for API failures.

## System & CLI Errors

These errors occur before the API server can successfully mount or during developer tooling operations.

### `Syntax Error in DSL`
Emitted during `caqui api start`, `push`, or `migrate` when the `schema.cq` file violates the core `pest` grammar rules (e.g., missing brackets, invalid keywords).
- **Resolution:** Correct the syntax in `schema.cq` according to the schema DSL documentation.

### `Semantic Error in DSL`
Emitted by the internal AST validator after successful parsing. This indicates a logical flaw in the schema design.
- **Examples:** Mismatched relation types, invalid shadowing in inheritance, or `@@fulltext` referencing non-existent fields.
- **Resolution:** Review the specific error string provided in the CLI output to locate and resolve the semantic violation in `schema.cq`.

### `Error: 'app.db' not found`
Emitted by `caqui api start`. The engine requires a physical SQLite database file to mount the router.
- **Resolution:** Run `caqui schema push` or `caqui schema migrate` to initialize the database from your schema before starting the API.

### `Git Proxy Enforcement`
Emitted when attempting to override the custom `-s sqlitevfs` strategy during a `caqui git merge` operation.
- **Message:** `Error: caqui enforces the 'sqlitevfs' merge driver for database integrity. You cannot override it with a custom strategy.`
- **Resolution:** Do not pass the `-s` or `--strategy` flags when using the `caqui git merge` proxy.

## API Error Responses

When a request fails, the API returns one of the following HTTP status codes alongside a plain-text message describing the failure.

*(Note: Future versions of the engine will migrate to structured JSON error objects).*

### 400 Bad Request (Validation)
The request payload was malformed, contained invalid types, or violated schema rules.
- **Examples:**
  - `Invalid field '...' for model '...'`: A requested field does not exist in the schema.
  - `Missing 'select' projection block`: All top-level actions (including mutations) require a `select` block to define the return shape.
  - `Security Exception: Model '...' undefined`: The requested model does not exist.
  - `Arrays cannot be optional` / `Primary keys cannot be optional`: These constraints are strictly enforced at the API layer.
  - `Security Exception: Maximum query depth exceeded.`: The nested graph traversal exceeds the configured engine limits (default 10 levels).
  - `Validation Error: Value '...' is not a valid variant for enum '...'`: You attempted to insert or update an enum field with an undefined string.
  - `Security Exception: Upsert target '...' is not marked as @id or @unique`: Nested and root upserts require the `where` block to target a strictly unique identifier.
  - `Unsupported action for polymorphic field '...'. Only 'connect' and 'create' are supported.`: You cannot perform an `update` or `delete` directly through a polymorphic relation; these must be done via root mutations.

### 405 Method Not Allowed (Security)
The requested action violates an internal security or architectural constraint.
- **Example:**
  - `Security Exception: Cannot mutate abstract bases directly.`: You attempted a root-level `create`, `update`, or `delete` on an abstract `base`. 
  - **Resolution:** Polymorphic bases cannot be instantiated directly. You must perform root mutations on the concrete models that `extend` the base.

### 500 Internal Server Error (Execution)
The request passed all semantic validation but failed during transactional execution against the SQLite database.

**1. Database Constraints:**
- `Database Error: UNIQUE constraint failed: ...`: Attempted to insert or update a value that conflicts with an `@unique` field constraint.
- `Mutation Execution Error: FOREIGN KEY constraint failed`: A nested mutation or connection violated referential integrity (e.g., attempting to connect to an ID that does not exist).

**2. Strict Execution Failures:**
- `Execution Error: Record not found`: Triggered by strict root-level `update` or `delete` actions where the provided `where` clause matches zero rows. The engine strictly requires these actions to target an existing record.

### 501 Not Implemented
The requested action is recognized by the AST but is not yet supported by the `api-layer`.
- **Example:**
  - `Mutation logic`: Returned when attempting bulk operations (e.g., `createMany`, `updateMany`) which are currently slated for future roadmap releases.
