use clap::{Parser, Subcommand};
use schema_parser::parser;
use schema_mapper::workflows;
use api_layer::{state::EngineState, router};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "caqui", version = "1.0", about = "Unified Schema-Driven SQLite Platform")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
#[command(rename_all = "kebab-case")]
enum Commands {
    Init,
    Git {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Schema {
        #[command(subcommand)]
        command: SchemaCommands,
    },
    Api {
        #[command(subcommand)]
        command: ApiCommands,
    },
}

#[derive(Subcommand)]
#[command(rename_all = "kebab-case")]
enum SchemaCommands {
    DbPush,
    MigrateDev,
}

#[derive(Subcommand)]
#[command(rename_all = "kebab-case")]
enum ApiCommands {
    Start,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Commands::Init = cli.command {
        if std::path::Path::new("schema.cq").exists() {
            println!("schema.cq already exists.");
        } else {
            let default_schema = r#"model User {
  id    String @id @default(uuid())
  name  String
  posts Post[]
}

model Post {
  id       String @id @default(uuid())
  title    String
  authorId String
  author   User   @relation(fields: [authorId], references: [id])
}

union SearchResult = User | Post
"#;
            std::fs::write("schema.cq", default_schema).unwrap();
            println!("SUCCESS: Created default schema.cq!");
            println!("Next steps: run `caqui schema db-push` or `caqui schema migrate-dev` to apply the schema.");
        }
        return;
    }

    // 1. Phase 1: Bootstrap the Custom C-FFI Virtual File System
    engine_core::vfs::bootstrap_custom_vfs();

    // 2. Phase 2: Parse the DSL into Memory dynamically
    let schema_text = std::fs::read_to_string("schema.cq").unwrap_or_else(|_| "model User { id String @id }".to_string());
    let desired_ast = parser::parse_schema(&schema_text).expect("Syntax Error in DSL");
    
    // 3. Spin up the SQLite connection pool using the Custom VFS & WAL pragmas
    let db_pool = engine_core::pool::create_pool("file:app.db?vfs=git");

    match cli.command {
        Commands::Init => unreachable!(),
        Commands::Git { args } => {
            engine_core::git::proxy_git_command(args);
        }
        Commands::Schema { command } => match command {
            SchemaCommands::DbPush => {
                // Phase 3: Push non-destructive Schema Diffs directly to SQLite
                let desired_ir = schema_mapper::lower_ast_to_physical(&desired_ast);
                let conn = rusqlite::Connection::open_with_flags(
                    "file:app.db?vfs=git",
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE | rusqlite::OpenFlags::SQLITE_OPEN_URI,
                ).unwrap();
                
                workflows::db_push(&conn, &desired_ir).unwrap();
                println!("SUCCESS: Database schema synced.");
            }
            SchemaCommands::MigrateDev => {
                // Phase 3: Spawn Shadow Database, Introspect, and generate safe .sql files
                let desired_ir = schema_mapper::lower_ast_to_physical(&desired_ast);
                workflows::migrate_dev(&desired_ir, "file:app.db?vfs=git", "migrations").unwrap();
                println!("SUCCESS: Database migrations generated and synced.");
            }
        },
        Commands::Api { command } => match command {
            ApiCommands::Start => {
                // Phase 5: Build Global Application State
                let state = EngineState {
                    db_pool,
                    ast: Arc::new(desired_ast),
                };

                // Phase 5: Mount Dynamic API and Bind to Port
                let app_router = router::build_dynamic_router(state);
                let listener = tokio::net::TcpListener::bind("0.0.0.0:4000").await.unwrap();
                
                println!("SUCCESS: Unified Engine running on http://0.0.0.0:4000/api/v1/query");
                axum::serve(listener, app_router).await.unwrap();
            }
        }
    }
}
