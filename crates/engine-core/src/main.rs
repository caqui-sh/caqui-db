use clap::{Parser, Subcommand};
use schema_parser::parser;
use schema_mapper::workflows;
use api_layer::{state::EngineState, router};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "DataEngine", version = "1.0", about = "Unified Schema-Driven SQLite Platform")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Start,
    DbPush,
    MigrateDev,
}

#[tokio::main]
async fn main() {
    // 1. Phase 1: Bootstrap the Custom C-FFI Virtual File System
    engine_core::vfs::bootstrap_custom_vfs();

    // 2. Phase 2: Parse the DSL into Memory dynamically
    let schema_text = std::fs::read_to_string("schema.dsl").unwrap_or_else(|_| "model User { id String @id }".to_string());
    let desired_ast = parser::parse_schema(&schema_text).expect("Syntax Error in DSL");
    
    let cli = Cli::parse();
    
    // 3. Spin up the SQLite connection pool using the Custom VFS & WAL pragmas
    // Normally, this would use deadpool_sqlite with custom config, but we'll mock a simple setup for the architecture flow
    let cfg = deadpool_sqlite::Config::new("file:app.db?vfs=git");
    let db_pool = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap();

    match cli.command {
        Commands::DbPush => {
            // Phase 3: Push non-destructive Schema Diffs directly to SQLite
            let desired_ir = schema_mapper::lower_ast_to_physical(&desired_ast);
            let conn = rusqlite::Connection::open("file:app.db?vfs=git").unwrap();
            
            workflows::db_push(&conn, &desired_ir).unwrap();
            println!("SUCCESS: Database schema synced.");
        }
        Commands::MigrateDev => {
            // Phase 3: Spawn Shadow Database, Introspect, and generate safe .sql files
            let desired_ir = schema_mapper::lower_ast_to_physical(&desired_ast);
            workflows::migrate_dev(&desired_ir, "file:app.db?vfs=git", "migrations").unwrap();
            println!("SUCCESS: Database migrations generated and synced.");
        }
        Commands::Start => {
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
