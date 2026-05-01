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
