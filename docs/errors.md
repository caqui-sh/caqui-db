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
  - `Missing 'select' projection block`: All `findMany` and mutation actions require a `select` block.
  - `Security Exception: Prohibited write to ignored field 'secret'`: You attempted to write to a field marked with `@ignore`.
  - `Security Exception: Model 'Ghost' undefined`: You requested a model not found in the schema.

### 500 Internal Server Error
The request was valid, but an error occurred during execution in the database layer.
- **Examples:**
  - `Database Error: UNIQUE constraint failed: User.email`: An attempt to insert or update a value that conflicts with a unique constraint.
  - `Record not found`: You attempted to `update` or `delete` a record that does not exist in the database.
  - `Mutation Execution Error: FOREIGN KEY constraint failed`: A nested mutation violated referential integrity.

### 501 Not Implemented
The requested action is recognized by the engine but is either not supported for that model or not yet implemented.
- **Example:**
  - `Mutation logic`: Returned when an action like `createMany` is sent but not yet supported by the current engine version.

## Error Payload Shape

Currently, `caqui` returns error messages as a **plain text string** in the response body.

```bash
HTTP/1.1 400 Bad Request
Content-Type: text/plain

Invalid field 'unknown_field' for model 'User'.
```

Future versions of the engine will migrate to structured JSON error objects (e.g., `{ "errors": [{ "message": "...", "code": "..." }] }`).
