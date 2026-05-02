# Error Handling & API Responses

`caqui` uses standard HTTP status codes and descriptive plain-text error messages to communicate failures.

## Success Responses

A successful request always returns an HTTP `200 OK` status with a JSON object containing the requested data under a `data` key.

```json
{
  "data": {
    "id": "u1",
    "name": "Alice"
  }
}
```

## Error Status Codes

When a request fails, `caqui` returns one of the following status codes:

### 400 Bad Request
The request was malformed or violated schema rules.
- **Examples:**
  - `Invalid field 'unknown_field' for model 'User'`: You requested a field that doesn't exist in your `schema.cq`.
    - **Actionable Resolution:** Check your `schema.cq` file to ensure the field is defined exactly as requested.
  - `Missing 'select' projection block`: All `findMany` and mutation actions require a `select` block.
    - **Actionable Resolution:** Add a `select` block to your query payload to specify the fields you want returned.
  - `Security Exception: Model 'Ghost' undefined`: You requested a model not found in the schema.
    - **Actionable Resolution:** Verify the model name in your query matches a defined model in `schema.cq`.
  - `Arrays cannot be optional`: You defined an array field in a way that allows it to be null, which is not supported.
    - **Actionable Resolution:** Update your `schema.cq` to ensure array fields are always required (e.g., `[String]` instead of `[String]?`).
  - `Primary keys cannot be optional`: A primary key field was marked as optional.
    - **Actionable Resolution:** Update your `schema.cq` to make the primary key field required.
  - `Security Exception: Maximum query depth exceeded.`: The requested query is too deeply nested.
    - **Actionable Resolution:** Reduce the nesting depth of your JSON query to remain within the allowed limits.

### 500 Internal Server Error
The request was valid, but an error occurred during execution in the database layer.
- **Examples:**
  - `Database Error: UNIQUE constraint failed: User.email`: An attempt to insert or update a value that conflicts with a unique constraint.
    - **Actionable Resolution:** Ensure the value you are providing for the unique field does not already exist in the system.
  - `Record not found`: You attempted to `update` or `delete` a record that does not exist in the database.
    - **Actionable Resolution:** Verify that the ID or lookup condition matches an existing record before attempting the operation.
  - `Mutation Execution Error: FOREIGN KEY constraint failed`: A nested mutation violated referential integrity.
    - **Actionable Resolution:** Ensure that any referenced related records exist before creating or updating the relationship.

### 501 Not Implemented
The requested action is recognized by the engine but is either not supported for that model or not yet implemented.
- **Example:**
  - `Mutation logic`: Returned when an action like `createMany` is sent but not yet supported by the current engine version.
    - **Actionable Resolution:** Use an alternative approach, such as issuing multiple individual `create` requests, or wait for a future update.

## Error Payload Shape

Currently, `caqui` returns error messages as a **plain text string** in the response body.

```bash
HTTP/1.1 400 Bad Request
Content-Type: text/plain

Invalid field 'unknown_field' for model 'User'.
```

Future versions of the engine will migrate to structured JSON error objects (e.g., `{ "errors": [{ "message": "...", "code": "..." }] }`).