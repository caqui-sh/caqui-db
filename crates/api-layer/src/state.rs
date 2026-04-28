use deadpool_sqlite::Pool;
use schema_parser::ast::SchemaAst;
use std::sync::Arc;

#[derive(Clone)]
pub struct EngineState {
    pub ast: Arc<SchemaAst>, // Immutable, heavily concurrent read-access for validation
    pub db_pool: Pool,       // Deadpool's Pool is already an Arc internally
}
