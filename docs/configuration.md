# Configuration & Engine Pragmas

`caqui` is designed to be zero-config for many use cases, but it provides several hooks for runtime customization and enforces strict database behaviors to ensure data integrity.

## Environment Variables

The `caqui` binary respects the following environment variables:

| Variable | Description | Default |
| :--- | :--- | :--- |
| `PORT` | The TCP port the API server binds to. | `4000` |
| `DATABASE_URL` | Reserved for future use (currently defaults to `file:app.db?vfs=git`). | N/A |

## Database Connection Pragmas

To guarantee safety in a decentralized environment, the `caqui` engine automatically configures every SQLite connection in the pool with the following settings:

### Foreign Key Enforcement
```sql
PRAGMA foreign_keys = ON;
```
`caqui` strictly enforces referential integrity at the database level. Any mutation that violates a foreign key constraint (e.g., trying to link to a record that doesn't exist) will trigger a transaction rollback and return a `500 Internal Server Error`.

### Write-Ahead Logging (WAL)
The engine is configured to use WAL mode. This allows for concurrent readers and a single writer, improving performance in high-concurrency environments.

## Custom SQLite Functions

`caqui` injects custom C-FFI functions into the SQLite runtime to provide features not available in standard SQLite.

### `gen_uuid7()`
This function is used by the `@default(uuid())` schema attribute.
- **Sequential:** Unlike random UUIDv4, UUIDv7 is time-ordered.
- **Performance:** Because it is sequential, it prevents "index fragmentation" in SQLite, leading to significantly faster insertion and lookup performance in large tables.
- **Usage:** You can also call this function directly in your migration scripts if needed.

```sql
INSERT INTO User (id, name) VALUES (gen_uuid7(), 'Alice');
```
