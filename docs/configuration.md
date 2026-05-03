# Configuration

`caqui` is designed to be "zero-config" for most use cases. It prioritizes convention over configuration to ensure data integrity within its decentralized Git model.

## Environment Variables

The `caqui` binary relies almost entirely on convention, but it respects the following environment variables for runtime deployment:

| Variable | Description | Default |
| :--- | :--- | :--- |
| `PORT` | The TCP port the API server binds to. | `4000` |
| `DATABASE_URL` | **Reserved/Unsupported**. The engine strictly binds to `file:app.db?vfs=git` to enforce the Git proxy model. Providing a custom URL is currently ignored. | N/A |
