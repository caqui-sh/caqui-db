use axum::{extract::State, extract::Json, response::IntoResponse, http::StatusCode, http::header};
use serde_json::Value;
use crate::state::EngineState;
use crate::translator::hydrate_payload_to_ir;

pub async fn api_execution_handler(
    State(state): State<EngineState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    
    let model = payload["model"].as_str().unwrap_or_default();
    let action = payload["action"].as_str().unwrap_or_default();

    if action == "findMany" {
        let mut alias_idx = 0;
        
        // 1. Validate & Hydrate to IR (Phase 5)
        let query_ir = match hydrate_payload_to_ir(&state.ast, model, &payload, &mut alias_idx, 0) {
            Ok(ir) => ir,
            Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
        };

        // 2. Compile IR to a single JSON-aggregating SQL string (Phase 4)
        let sql_query = query_compiler::read::compile_select(&query_ir, None);

        // 3. Thread-safe execution against the custom VFS-backed SQLite pool
        let conn = state.db_pool.get().await.unwrap();
        
        // `interact` pushes the blocking SQLite C-FFI call to a dedicated thread, 
        // preventing Tokio async worker starvation.
        let raw_json_string = conn.interact(move |db| {
            db.prepare_cached(&sql_query)
                .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, String>(0)))
        }).await;

        match raw_json_string {
            Ok(Ok(json_payload)) => {
                // 4. Zero-Overhead HTTP Proxying: Do not deserialize into Rust structs!
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    format!("{{\"data\": {}}}", json_payload), 
                ).into_response()
            },
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Database Error: {}", e)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Execution Error: {}", e)).into_response(),
        }
    } else if action == "create" || action == "update" || action == "delete" || action == "upsert" {
        let mut alias_idx = 0;
        
        // 1. If it's a delete, we MUST fetch the data before it's gone
        let mut deleted_data: Option<String> = None;
        if action == "delete" {
            let select_block = payload.get("select").cloned().unwrap_or_else(|| serde_json::json!({ "id": true }));
            let temp_payload = serde_json::json!({
                "select": select_block,
                "where": payload.get("where")
            });
            let mut read_alias_idx = 0;
            if let Ok(query_ir) = hydrate_payload_to_ir(&state.ast, model, &temp_payload, &mut read_alias_idx, 0) {
                let sql_query = query_compiler::read::compile_select(&query_ir, None);
                let conn = state.db_pool.get().await.unwrap();
                let raw_json_string = conn.interact(move |db| {
                    db.prepare_cached(&sql_query)
                        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, String>(0)))
                }).await;
                if let Ok(Ok(json)) = raw_json_string {
                    deleted_data = Some(json);
                }
            }
        }

        // 2. Hydrate the mutation to a Linear Execution Plan
        let plan = match crate::mutation_translator::hydrate_mutation_to_plan(&state.ast, model, action, &payload, &mut alias_idx) {
            Ok(p) => p,
            Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
        };
        
        // 3. Execute the mutation transactionally
        let root_id = match crate::executor::execute_mutation_plan(&state.db_pool, plan).await {
            Ok(id) => id,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Mutation Execution Error: {}", e)).into_response(),
        };
        
        let raw_json_string = if action == "delete" {
            Ok(Ok(deleted_data.unwrap_or_else(|| format!("[{{\"id\": \"{}\"}}]", root_id))))
        } else {
            // 4. Create a temporary synthetic read payload to fetch the mutated record
            let select_block = payload.get("select").cloned().unwrap_or_else(|| serde_json::json!({ "id": true }));
            let temp_payload = serde_json::json!({
                "select": select_block,
                "where": {
                    "id": root_id
                }
            });
            
            let mut read_alias_idx = 0;
            let query_ir = match hydrate_payload_to_ir(&state.ast, model, &temp_payload, &mut read_alias_idx, 0) {
                Ok(ir) => ir,
                Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
            };
            
            let sql_query = query_compiler::read::compile_select(&query_ir, None);
            
            let conn = state.db_pool.get().await.unwrap();
            conn.interact(move |db| {
                db.prepare_cached(&sql_query)
                    .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, String>(0)))
            }).await
        };

        match raw_json_string {
            Ok(Ok(json_payload)) => {
                let final_data = if action == "create" || action == "update" || action == "delete" || action == "upsert" {
                    let val: serde_json::Value = serde_json::from_str(&json_payload).unwrap_or(serde_json::Value::Null);
                    if let Some(first) = val.as_array().and_then(|a| a.first()) {
                        serde_json::to_string(first).unwrap_or(json_payload)
                    } else {
                        json_payload
                    }
                } else {
                    json_payload
                };
                
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    format!("{{\"data\": {}}}", final_data), 
                ).into_response()
            },
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Database Read Error after Mutation: {}", e)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Execution Error: {}", e)).into_response(),
        }
    } else {
        (StatusCode::NOT_IMPLEMENTED, "Mutation logic").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
        routing::post,
    };
    use tower::ServiceExt; // for `call`, `into_service`, and `oneshot`
    use std::sync::Arc;
    use schema_parser::ast::{SchemaAst, ModelNode, FieldNode, AstFieldType, FieldAttribute, DefaultFunc};
    use std::collections::HashMap;

    use std::sync::atomic::{AtomicUsize, Ordering};

    static DB_COUNTER: AtomicUsize = AtomicUsize::new(0);

    async fn build_test_state() -> EngineState {
        // Setup an AST
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };

        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::Uuid)] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Unique] },
                FieldNode { name: "age".to_string(), field_type: AstFieldType::Scalar("Int".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "bio".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "secret".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Ignore] },
                FieldNode { name: "tags".to_string(), field_type: AstFieldType::ScalarArray("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "updated_at".to_string(), field_type: AstFieldType::Scalar("DateTime".to_string()), is_optional: false, attributes: vec![FieldAttribute::UpdatedAt] },
                FieldNode { name: "posts".to_string(), field_type: AstFieldType::RelationArray("Post".to_string()), is_optional: false, attributes: vec![
                    FieldAttribute::Relation {
                        name: None,
                        fields: vec![],
                        references: vec![],
                        on_delete: None,
                        deferrable: false,
                        column: None,
                    }
                ] },
                FieldNode { name: "profileId".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "profile".to_string(), field_type: AstFieldType::Relation("Profile".to_string()), is_optional: false, attributes: vec![
                    FieldAttribute::Relation {
                        name: None,
                        fields: vec!["profileId".to_string()],
                        references: vec!["id".to_string()],
                        on_delete: None,
                        deferrable: false,
                        column: None,
                    }
                ] },
            ]
        });

        ast.models.insert("Profile".to_string(), ModelNode {
            name: "Profile".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::Uuid)] },
                FieldNode { name: "bio".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
            ]
        });

        ast.models.insert("Post".to_string(), ModelNode {
            name: "Post".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::Uuid)] },
                FieldNode { name: "title".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "authorId".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "author".to_string(), field_type: AstFieldType::Relation("User".to_string()), is_optional: false, attributes: vec![
                    FieldAttribute::Relation {
                        name: None,
                        fields: vec!["authorId".to_string()],
                        references: vec!["id".to_string()],
                        on_delete: Some("Cascade".to_string()),
                        deferrable: false,
                        column: None,
                    }
                ] },
                FieldNode { name: "comments".to_string(), field_type: AstFieldType::RelationArray("Comment".to_string()), is_optional: false, attributes: vec![
                    FieldAttribute::Relation {
                        name: None,
                        fields: vec![],
                        references: vec![],
                        on_delete: None,
                        deferrable: false,
                        column: None,
                    }
                ] },
            ]
        });

        ast.models.insert("Comment".to_string(), ModelNode {
            name: "Comment".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::Uuid)] },
                FieldNode { name: "text".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "postId".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "post".to_string(), field_type: AstFieldType::Relation("Post".to_string()), is_optional: false, attributes: vec![
                    FieldAttribute::Relation {
                        name: None,
                        fields: vec!["postId".to_string()],
                        references: vec!["id".to_string()],
                        on_delete: Some("Cascade".to_string()),
                        deferrable: false,
                        column: None,
                    }
                ] },
            ]
        });

        ast.models.insert("Employee".to_string(), ModelNode {
            name: "Employee".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::Uuid)] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "managerId".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: true, attributes: vec![] },
                FieldNode { name: "manager".to_string(), field_type: AstFieldType::Relation("Employee".to_string()), is_optional: true, attributes: vec![
                    FieldAttribute::Relation {
                        name: Some("Management".to_string()),
                        fields: vec!["managerId".to_string()],
                        references: vec!["id".to_string()],
                        on_delete: None,
                        deferrable: false,
                        column: None,
                    }
                ] },
                FieldNode { name: "subordinates".to_string(), field_type: AstFieldType::RelationArray("Employee".to_string()), is_optional: false, attributes: vec![
                    FieldAttribute::Relation {
                        name: Some("Management".to_string()),
                        fields: vec![],
                        references: vec![],
                        on_delete: None,
                        deferrable: false,
                        column: None,
                    }
                ] },
            ]
        });

        // Setup the Deadpool sqlite using a unique shared cache name for isolation between tests
        let db_name = format!("file:memdb{}?mode=memory&cache=shared", DB_COUNTER.fetch_add(1, Ordering::SeqCst));
        let pool = crate::db::create_pool(&db_name);

        // Seed DB directly through the pool
        let conn = pool.get().await.unwrap();
        conn.interact(|db| -> Result<(), rusqlite::Error> {
            db.execute("CREATE TABLE Employee (
                id TEXT PRIMARY KEY DEFAULT (gen_uuid7()),
                name TEXT NOT NULL,
                managerId TEXT REFERENCES Employee(id)
            ) STRICT;", [])?;
            db.execute("CREATE TABLE Profile (
                id TEXT PRIMARY KEY DEFAULT (gen_uuid7()),
                bio TEXT
            ) STRICT;", [])?;

            db.execute("CREATE TABLE User (
                id TEXT PRIMARY KEY DEFAULT (gen_uuid7()), 
                name TEXT NOT NULL UNIQUE, 
                age INTEGER, 
                bio TEXT,
                secret TEXT,
                tags TEXT,
                updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
                profileId TEXT,
                FOREIGN KEY(profileId) REFERENCES Profile(id) ON DELETE SET NULL
            ) STRICT;", [])?;

            db.execute("CREATE TABLE Post (
                id TEXT PRIMARY KEY DEFAULT (gen_uuid7()), 
                title TEXT NOT NULL, 
                authorId TEXT NOT NULL,
                FOREIGN KEY(authorId) REFERENCES User(id) ON DELETE CASCADE
            ) STRICT;", [])?;

            db.execute("CREATE TABLE Comment (
                id TEXT PRIMARY KEY DEFAULT (gen_uuid7()),
                text TEXT NOT NULL,
                postId TEXT NOT NULL,
                FOREIGN KEY(postId) REFERENCES Post(id) ON DELETE CASCADE
            ) STRICT;", [])?;

            db.execute("CREATE TRIGGER trg_user_updated_at 
                AFTER UPDATE ON User 
                FOR EACH ROW 
                BEGIN 
                    UPDATE User SET updated_at = CURRENT_TIMESTAMP WHERE id = OLD.id; 
                END;", [])?;

            db.execute("INSERT INTO Profile (id, bio) VALUES ('prof1', 'Existing Profile');", [])?;
            db.execute("INSERT INTO User (id, name, age, bio, secret, tags, updated_at, profileId) VALUES ('u1', 'Bob', 25, 'Original Bio', 'Hidden', '[\"rust\", \"sql\"]', '2020-01-01 00:00:00', 'prof1');", [])?;
            db.execute("INSERT INTO Post (id, title, authorId) VALUES ('p1', 'First Post', 'u1');", [])?;
            db.execute("INSERT INTO Comment (id, text, postId) VALUES ('c1', 'First Comment', 'p1');", [])?;
            Ok(())
        }).await.unwrap().unwrap();

        EngineState {
            ast: Arc::new(ast),
            db_pool: pool,
        }
    }

    #[tokio::test]
    async fn test_api_execution_handler_valid() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "select": {
                        "id": true,
                        "name": true
                    }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let users = json_body["data"].as_array().expect("Expected data to be an array");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0]["id"], "u1");
        assert_eq!(users[0]["name"], "Bob");
    }

    #[tokio::test]
    async fn test_api_execution_handler_bad_request() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "select": {
                        "unknown_field": true
                    }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Invalid field 'unknown_field'"));
    }

    #[tokio::test]
    async fn test_api_execution_handler_unsupported_action() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "createMany",
                    "data": []
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_api_execution_handler_db_error() {
        let mut state = build_test_state().await;
        
        // Add a model to AST that DOES NOT have a table in DB
        let mut ast = (*state.ast).clone();
        ast.models.insert("Ghost".to_string(), ModelNode {
            name: "Ghost".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
            ]
        });
        state.ast = Arc::new(ast);

        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "Ghost",
                    "action": "findMany",
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should fail because table 'Ghost' doesn't exist
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_api_execution_handler_missing_model() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "action": "findMany",
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Security Exception: Model '' undefined"));
    }

    #[tokio::test]
    async fn test_api_execution_handler_missing_action() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_api_execution_handler_missing_select() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany"
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Missing 'select' projection block"));
    }

    #[tokio::test]
    async fn test_api_execution_handler_create() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "id": "u2",
                        "name": "Alice"
                    },
                    "select": {
                        "id": true,
                        "name": true
                    }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        assert_eq!(json_body["data"]["id"], "u2");
        assert_eq!(json_body["data"]["name"], "Alice");
    }

    #[tokio::test]
    async fn test_api_execution_handler_update() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "name": "Bobby"
                    },
                    "select": {
                        "id": true,
                        "name": true
                    }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        assert_eq!(json_body["data"]["id"], "u1");
        assert_eq!(json_body["data"]["name"], "Bobby");
    }

    #[tokio::test]
    async fn test_api_execution_handler_delete() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "delete",
                    "where": { "id": "u1" },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json_body["data"]["id"], "u1");

        // Verify it's gone
        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM User WHERE id = 'u1'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_api_mutation_auto_id_generation() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "AutoIDUser"
                    },
                    "select": { "id": true, "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let generated_id = json_body["data"]["id"].as_str().expect("Expected a generated ID");
        assert_eq!(json_body["data"]["name"], "AutoIDUser");
        assert!(generated_id.len() > 10, "ID should be a long string (UUID)");
    }

    #[tokio::test]
    async fn test_api_mutation_rollback_on_conflict() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        // Attempt to create a user with duplicate name 'Bob'
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "id": "u2",
                        "name": "Bob" 
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // Verify that 'u2' was NOT created due to rollback
        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM User WHERE id = 'u2'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_api_mutation_security_ignore_enforcement() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        // Attempt to write to 'secret' which is marked with @ignore
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "Hacker",
                        "secret": "MALICIOUS"
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Security Exception: Prohibited write to ignored field 'secret'"));
    }

    #[tokio::test]
    async fn test_api_mutation_update_null_value() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        // Set bio to null
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "bio": null
                    },
                    "select": { "id": true, "bio": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        assert!(json_body["data"]["bio"].is_null());
    }

    #[tokio::test]
    async fn test_api_mutation_type_strictness() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        // Send string for age (which is Int)
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "OldMan",
                        "age": "Ninety"
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Since we don't have upfront validation yet, it should fail during SQL binding/execution
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_api_mutation_missing_record() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "does_not_exist" },
                    "data": { "age": 99 },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Record not found"));
    }

    #[tokio::test]
    async fn test_api_mutation_updated_at_verification() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        // Get initial updated_at
        let conn = state.db_pool.get().await.unwrap();
        let initial_time: String = conn.interact(|db| {
            db.query_row("SELECT updated_at FROM User WHERE id = 'u1'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        
        // Wait a tiny bit (simulated via update)
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": { "age": 30 },
                    "select": { "id": true, "updated_at": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let new_time = json_body["data"]["updated_at"].as_str().unwrap();
        
        assert_ne!(initial_time, new_time, "updated_at timestamp should have changed");
    }

    #[tokio::test]
    async fn test_api_mutation_selective_output() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": { "name": "Minimalist" },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let data = json_body["data"].as_object().unwrap();
        assert!(data.contains_key("name"));
        assert!(!data.contains_key("id"), "Output should only contain requested projection");
    }

    #[tokio::test]
    async fn test_api_mutation_complex_where() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "delete",
                    "where": {
                        "AND": [
                            { "name": { "eq": "Bob" } },
                            { "age": { "eq": 25 } }
                        ]
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM User WHERE name = 'Bob'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_api_mutation_string_safety() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let malicious_name = "O'Reilly 🍕; DROP TABLE User;";
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                format!(r#"{{
                    "model": "User",
                    "action": "create",
                    "data": {{ "name": "{}" }},
                    "select": {{ "name": true }}
                }}"#, malicious_name)
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        assert_eq!(json_body["data"]["name"].as_str().unwrap(), malicious_name, "Unicode and quotes should be preserved safely");
    }

    #[tokio::test]
    async fn test_api_nested_create() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "Alice",
                        "posts": {
                            "create": [
                                { "title": "Alice's First Post" },
                                { "title": "Alice's Second Post" }
                            ]
                        }
                    },
                    "select": { "id": true, "name": true, "posts": { "select": { "title": true } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let data = json_body["data"].as_object().unwrap();
        assert_eq!(data["name"], "Alice");
        let posts = data["posts"].as_array().unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0]["title"], "Alice's First Post");
        assert_eq!(posts[1]["title"], "Alice's Second Post");
    }

    #[tokio::test]
    async fn test_api_nested_connect() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "Post",
                    "action": "create",
                    "data": {
                        "title": "A post for Bob",
                        "author": {
                            "connect": {
                                "id": "u1"
                            }
                        }
                    },
                    "select": { "id": true, "title": true, "author": { "select": { "name": true } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let data = json_body["data"].as_object().unwrap();
        assert_eq!(data["title"], "A post for Bob");
        let author = data["author"].as_object().unwrap();
        assert_eq!(author["name"], "Bob");
    }

    #[tokio::test]
    async fn test_api_nested_create_rollback_on_child_failure() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "Eve",
                        "posts": {
                            "create": [
                                { "title": "Eve's Valid Post" },
                                { "title": null } 
                            ]
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // The second post has title: null, which violates NOT NULL constraint on Post.title
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // Verify that 'Eve' was NOT created due to full graph rollback
        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM User WHERE name = 'Eve'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_api_nested_connect_child_holds_fk() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "Charlie",
                        "posts": {
                            "connect": [{ "id": "p1" }]
                        }
                    },
                    "select": { "id": true, "name": true, "posts": { "select": { "id": true } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify the post was actually connected to Charlie
        let conn = state.db_pool.get().await.unwrap();
        let author_name: String = conn.interact(|db| {
            db.query_row(
                "SELECT User.name FROM Post JOIN User ON Post.authorId = User.id WHERE Post.id = 'p1'", 
                [], 
                |row| row.get(0)
            )
        }).await.unwrap().unwrap();
        assert_eq!(author_name, "Charlie", "Post p1 should now belong to Charlie");
    }

    #[tokio::test]
    async fn test_api_nested_mutations_inside_update() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "bio": "Updated Bio",
                        "posts": {
                            "create": [{ "title": "Bob's Second Post" }]
                        }
                    },
                    "select": { "id": true, "bio": true, "posts": { "select": { "title": true } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_object().unwrap();
        assert_eq!(data["bio"], "Updated Bio");

        // Verify the new post exists in the database
        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM Post WHERE title = 'Bob''s Second Post' AND authorId = 'u1'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 1, "The nested post should be created for u1");
    }

    #[tokio::test]
    async fn test_api_deeply_nested_recursion_3_levels() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "Dave",
                        "posts": {
                            "create": [
                                {
                                    "title": "Dave's First Post",
                                    "comments": {
                                        "create": [
                                            { "text": "Great post Dave!" }
                                        ]
                                    }
                                }
                            ]
                        }
                    },
                    "select": { "id": true, "name": true, "posts": { "select": { "title": true, "comments": { "select": { "text": true } } } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify that the comment was inserted into the database and linked properly
        let conn = state.db_pool.get().await.unwrap();
        let author_name: String = conn.interact(|db| {
            db.query_row(
                "SELECT User.name FROM Comment JOIN Post ON Comment.postId = Post.id JOIN User ON Post.authorId = User.id WHERE Comment.text = 'Great post Dave!'", 
                [], 
                |row| row.get(0)
            )
        }).await.unwrap().unwrap();
        assert_eq!(author_name, "Dave", "Comment should be linked to Dave's post");
    }

    #[tokio::test]
    async fn test_api_mixed_nested_actions() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "Eve",
                        "posts": {
                            "create": [{ "title": "Eve's New Post" }],
                            "connect": [{ "id": "p1" }]
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify Eve owns both the new post and the connected post
        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM Post JOIN User ON Post.authorId = User.id WHERE User.name = 'Eve'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 2, "Eve should own exactly 2 posts");
    }

    #[tokio::test]
    async fn test_api_1_to_1_relation_parent_holds_fk() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state);

        // Create User (Parent, holds profileId) -> Profile (Child)
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "create",
                    "data": {
                        "name": "Frank",
                        "profile": {
                            "create": { "bio": "Frank's Profile" }
                        }
                    },
                    "select": { "id": true, "profile": { "select": { "bio": true } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let data = json_body["data"].as_object().unwrap();
        let profile = data["profile"].as_object().unwrap();
        assert_eq!(profile["bio"], "Frank's Profile");
    }

    #[tokio::test]
    async fn test_api_nested_update() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "posts": {
                            "update": [{
                                "where": { "id": "p1" },
                                "data": { "title": "New Title" }
                            }]
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let conn = state.db_pool.get().await.unwrap();
        let updated_title: String = conn.interact(|db| {
            db.query_row("SELECT title FROM Post WHERE id = 'p1'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(updated_title, "New Title");
    }

    #[tokio::test]
    async fn test_api_nested_delete() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        {
            let conn = state.db_pool.get().await.unwrap();
            conn.interact(|db| {
                db.execute("INSERT INTO User (id, name) VALUES ('u2', 'Delete User')", []).unwrap();
                db.execute("INSERT INTO Post (id, title, authorId) VALUES ('p2', 'To Delete', 'u2')", []).unwrap();
            }).await.unwrap();
        }

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u2" },
                    "data": {
                        "posts": {
                            "delete": [{ "id": "p2" }]
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM Post WHERE id = 'p2'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_api_nested_disconnect() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "profile": {
                            "disconnect": true
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let conn = state.db_pool.get().await.unwrap();
        let profile_id: Option<String> = conn.interact(|db| {
            db.query_row("SELECT profileId FROM User WHERE id = 'u1'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert!(profile_id.is_none(), "profileId should be NULL");
    }

    #[tokio::test]
    async fn test_api_nested_connect_failure() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        {
            let conn = state.db_pool.get().await.unwrap();
            conn.interact(|db| {
                db.execute("INSERT INTO User (id, name) VALUES ('u4', 'Connect Fail User')", []).unwrap();
            }).await.unwrap();
        }

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u4" },
                    "data": {
                        "profile": {
                            "connect": { "id": "invalid_profile_id" }
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_ne!(response.status(), StatusCode::OK, "Connecting to an invalid ID should fail foreign key constraint");
    }

    #[tokio::test]
    async fn test_api_nested_set() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        {
            let conn = state.db_pool.get().await.unwrap();
            conn.interact(|db| {
                db.execute("INSERT INTO User (id, name) VALUES ('u5', 'Set User')", []).unwrap();
                db.execute("INSERT INTO User (id, name) VALUES ('u_other', 'Other User')", []).unwrap();
                db.execute("INSERT INTO Post (id, title, authorId) VALUES ('p5_new', 'New Post', 'u_other')", []).unwrap();
            }).await.unwrap();
        }

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u5" },
                    "data": {
                        "posts": {
                            "set": [{ "id": "p5_new" }]
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        if response.status() != StatusCode::OK {
            let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body_str = String::from_utf8_lossy(&body_bytes);
            panic!("Request failed with 500: {}", body_str);
        }
        assert_eq!(response.status(), StatusCode::OK);

        let conn = state.db_pool.get().await.unwrap();
        let author_id: String = conn.interact(|db| {
            db.query_row("SELECT authorId FROM Post WHERE id = 'p5_new'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(author_id, "u5");
    }

    #[tokio::test]
    async fn test_api_mutation_scalar_array_push() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "tags": { "push": "new_tag" }
                    },
                    "select": { "tags": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let tags = json_body["data"]["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 3); // "rust", "sql", "new_tag"
        assert_eq!(tags[2].as_str().unwrap(), "new_tag");
    }

    #[tokio::test]
    async fn test_api_nested_upsert() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "profile": {
                            "upsert": {
                                "create": { "bio": "New Bio" },
                                "update": { "data": { "bio": "Updated Bio" } }
                            }
                        }
                    },
                    "select": { "id": true, "profile": { "select": { "bio": true } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let profile = json_body["data"]["profile"].as_object().unwrap();
        assert_eq!(profile["bio"], "Updated Bio"); // It existed, so it should update
    }

    #[tokio::test]
    async fn test_api_nested_update_not_found_safety() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "posts": {
                            "update": [{ "where": { "id": "p_does_not_exist" }, "data": { "title": "hacked" } }]
                        }
                    },
                    "select": { "id": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // The nested update should fail, rolling back the transaction
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Record not found"));
    }

    #[tokio::test]
    async fn test_api_root_upsert() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "upsert",
                    "where": { "name": "Bob" },
                    "create": {
                        "name": "Bob",
                        "age": 20
                    },
                    "update": {
                        "age": 26
                    },
                    "select": { "name": true, "age": true }
                }"#
            ))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_object().unwrap();
        assert_eq!(data["name"], "Bob"); 
        assert_eq!(data["age"], 26); // Should have updated because Bob exists

        // Now test creation branch
        let request2 = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "upsert",
                    "where": { "name": "Alice" },
                    "create": {
                        "name": "Alice",
                        "age": 30
                    },
                    "update": {
                        "age": 31
                    },
                    "select": { "name": true, "age": true }
                }"#
            ))
            .unwrap();

        let response2 = app.oneshot(request2).await.unwrap();
        assert_eq!(response2.status(), StatusCode::OK);
        
        let body_bytes2 = axum::body::to_bytes(response2.into_body(), usize::MAX).await.unwrap();
        let json_body2: serde_json::Value = serde_json::from_slice(&body_bytes2).unwrap();
        let data2 = json_body2["data"].as_object().unwrap();
        assert_eq!(data2["name"], "Alice"); 
        assert_eq!(data2["age"], 30); // Should have created because Alice didn't exist
    }

    #[tokio::test]
    async fn test_api_root_upsert_nested_mutations() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "upsert",
                    "where": { "name": "Eve" },
                    "create": {
                        "name": "Eve",
                        "posts": {
                            "create": [{ "title": "Eve's First Post" }]
                        }
                    },
                    "update": {
                        "age": 35
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let conn = state.db_pool.get().await.unwrap();
        let count: i64 = conn.interact(|db| {
            db.query_row("SELECT COUNT(*) FROM Post JOIN User ON Post.authorId = User.id WHERE User.name = 'Eve' AND Post.title = 'Eve''s First Post'", [], |row| row.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 1, "The nested post should be created for Eve");
    }

    #[tokio::test]
    async fn test_api_root_upsert_empty_create() {
        // Upserting with auto-generated IDs and empty create block.
        // To do this, we need a model where all fields are optional or have defaults.
        // 'Profile' has 'id' (Default UUID) and 'bio' (Optional).
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "Profile",
                    "action": "upsert",
                    "where": { "id": "prof_auto" },
                    "create": {},
                    "update": {
                        "bio": "Updated Bio"
                    },
                    "select": { "id": true, "bio": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        
        if status != StatusCode::OK {
            let body_str = String::from_utf8_lossy(&body_bytes);
            panic!("Request failed with 500: {}", body_str);
        }
        
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_object().unwrap();
        // The id will be gen_uuid7() because 'create' is empty
        assert!(data["id"].as_str().unwrap() != "prof_auto");
        assert!(data["bio"].is_null() || data.get("bio").is_none());
    }

    #[tokio::test]
    async fn test_api_root_upsert_non_unique_target() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "upsert",
                    "where": { "age": 25 },
                    "create": {
                        "name": "NonUnique",
                        "age": 25
                    },
                    "update": {
                        "age": 26
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should fail gracefully from the Rust translation engine because 'age' is not UNIQUE
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("Security Exception: Upsert target 'age' is not marked as @id or @unique"));
    }

    #[tokio::test]
    async fn test_api_root_upsert_update_conflict_target() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "upsert",
                    "where": { "name": "Bob" },
                    "create": {
                        "name": "Bob"
                    },
                    "update": {
                        "name": "BobNew"
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_object().unwrap();
        assert_eq!(data["name"], "BobNew"); 
    }

    #[tokio::test]
    async fn test_api_relational_filtering_some() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "where": {
                        "posts": {
                            "some": { "title": { "eq": "First Post" } }
                        }
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "Bob"); 
    }

    #[tokio::test]
    async fn test_api_relational_filtering_none() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        // Add a second user with no posts
        let conn = state.db_pool.get().await.unwrap();
        conn.interact(|db| {
            db.execute("INSERT INTO User (id, name, age) VALUES ('u2', 'Alice', 30);", []).unwrap();
        }).await.unwrap();

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "where": {
                        "posts": {
                            "none": { "title": { "eq": "First Post" } }
                        }
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "Alice");
    }

    #[tokio::test]
    async fn test_api_relational_filtering_every() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        // Add a user with ONLY matching posts
        let conn = state.db_pool.get().await.unwrap();
        conn.interact(|db| {
            db.execute("INSERT INTO User (id, name, age) VALUES ('u_every', 'Every', 30);", []).unwrap();
            db.execute("INSERT INTO Post (id, title, authorId) VALUES ('p_e1', 'Good Post', 'u_every');", []).unwrap();
            db.execute("INSERT INTO Post (id, title, authorId) VALUES ('p_e2', 'Another Good Post', 'u_every');", []).unwrap();
            
            // User with NO posts (should evaluate to TRUE for every)
            db.execute("INSERT INTO User (id, name, age) VALUES ('u_empty', 'Empty', 30);", []).unwrap();
            
            // User with mixed posts (should evaluate to FALSE)
            db.execute("INSERT INTO User (id, name, age) VALUES ('u_mixed', 'Mixed', 30);", []).unwrap();
            db.execute("INSERT INTO Post (id, title, authorId) VALUES ('p_m1', 'Good Post', 'u_mixed');", []).unwrap();
            db.execute("INSERT INTO Post (id, title, authorId) VALUES ('p_m2', 'Bad Post', 'u_mixed');", []).unwrap();
        }).await.unwrap();

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "where": {
                        "posts": {
                            "every": { "title": { "eq": "Good Post" } }
                        }
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        // Wait, "Another Good Post" is not "Good Post". So 'u_every' actually evaluates to FALSE. Let's fix that query or expectation.
        // Actually, let's look for "Good" using like/contains. Since we only have eq, let's just test "Good Post".
        // We expect u_empty to return. 'Bob' (u1) has 'First Post', which is not 'Good Post', so Bob returns FALSE.
        // Wait, 'u_every' has 'p_e2' = 'Another Good Post', so it's FALSE. Let's make it TRUE.
        conn.interact(|db| {
            db.execute("UPDATE Post SET title = 'Good Post' WHERE id = 'p_e2';", []).unwrap();
        }).await.unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_array().unwrap();
        
        let names: Vec<&str> = data.iter().map(|v| v["name"].as_str().unwrap()).collect();
        assert_eq!(names.len(), 2, "Should find u_every and u_empty");
        assert!(names.contains(&"Every"));
        assert!(names.contains(&"Empty")); // Empty collections return true for EVERY
    }

    #[tokio::test]
    async fn test_api_relational_filtering_1_to_1() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "where": {
                        "profile": {
                            "is": { "bio": { "eq": "Existing Profile" } }
                        }
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "Bob"); 
    }

    #[tokio::test]
    async fn test_api_relational_filtering_deeply_nested() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "where": {
                        "posts": {
                            "some": {
                                "comments": {
                                    "some": { "text": { "eq": "First Comment" } }
                                }
                            }
                        }
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "Bob"); 
    }

    #[tokio::test]
    async fn test_api_relational_filtering_empty_existence() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        // Add a user with NO posts
        let conn = state.db_pool.get().await.unwrap();
        conn.interact(|db| {
            db.execute("INSERT INTO User (id, name, age) VALUES ('u2', 'Alice', 30);", []).unwrap();
        }).await.unwrap();

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "where": {
                        "posts": { "some": {} }
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let data = json_body["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["name"], "Bob"); // Alice has no posts
        
        let request2 = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "findMany",
                    "where": {
                        "posts": { "none": {} }
                    },
                    "select": { "name": true }
                }"#
            ))
            .unwrap();

        let response2 = app.oneshot(request2).await.unwrap();
        assert_eq!(response2.status(), StatusCode::OK);
        
        let body_bytes2 = axum::body::to_bytes(response2.into_body(), usize::MAX).await.unwrap();
        let json_body2: serde_json::Value = serde_json::from_slice(&body_bytes2).unwrap();
        let data2 = json_body2["data"].as_array().unwrap();
        assert_eq!(data2.len(), 1);
        assert_eq!(data2[0]["name"], "Alice"); // Alice is the only one with no posts
    }

    #[tokio::test]
    async fn test_api_relational_filtering_in_mutation() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "User",
                    "action": "update",
                    "where": { "id": "u1" },
                    "data": {
                        "posts": {
                            "update": [
                                {
                                    "where": { 
                                        "comments": { "some": { "text": { "eq": "First Comment" } } } 
                                    },
                                    "data": { "title": "Flagged" }
                                }
                            ]
                        }
                    },
                    "select": { "id": true, "posts": { "select": { "title": true } } }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let posts = json_body["data"]["posts"].as_array().unwrap();
        assert_eq!(posts[0]["title"].as_str().unwrap(), "Flagged"); 
    }

    #[tokio::test]
    async fn test_api_deeply_nested_hierarchical_create() {
        let state = build_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "Employee",
                    "action": "create",
                    "data": {
                        "name": "CEO",
                        "subordinates": {
                            "create": [
                                {
                                    "name": "Manager",
                                    "subordinates": {
                                        "create": [
                                            { "name": "Intern 1" },
                                            { "name": "Intern 2" }
                                        ]
                                    }
                                }
                            ]
                        }
                    },
                    "select": {
                        "id": true,
                        "name": true,
                        "subordinates": {
                            "select": {
                                "name": true,
                                "subordinates": {
                                    "select": {
                                        "name": true
                                    }
                                }
                            }
                        }
                    }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let root = &json_body["data"];
        assert_eq!(root["name"], "CEO");
        
        let manager = &root["subordinates"][0];
        assert_eq!(manager["name"], "Manager");
        
        let interns = manager["subordinates"].as_array().unwrap();
        assert_eq!(interns.len(), 2);
        assert!(interns.iter().any(|i| i["name"] == "Intern 1"));
        assert!(interns.iter().any(|i| i["name"] == "Intern 2"));
    }

    async fn build_scalar_test_state() -> EngineState {
        let mut ast = SchemaAst {
            models: HashMap::new(),
            unions: HashMap::new(),
        };

        ast.models.insert("Config".to_string(), ModelNode {
            name: "Config".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id, FieldAttribute::Default(DefaultFunc::Uuid)] },
                FieldNode { name: "isPublished".to_string(), field_type: AstFieldType::Scalar("Boolean".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "rating".to_string(), field_type: AstFieldType::Scalar("Float".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "scores".to_string(), field_type: AstFieldType::ScalarArray("Int".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "nickname".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: true, attributes: vec![] },
            ]
        });

        let db_name = format!("file:memdb{}?mode=memory&cache=shared", DB_COUNTER.fetch_add(1, Ordering::SeqCst));
        let pool = crate::db::create_pool(&db_name);

        let conn = pool.get().await.unwrap();
        conn.interact(|db| -> Result<(), rusqlite::Error> {
            db.execute("CREATE TABLE Config (
                id TEXT PRIMARY KEY DEFAULT (gen_uuid7()),
                isPublished INTEGER NOT NULL,
                rating REAL NOT NULL,
                scores TEXT NOT NULL,
                nickname TEXT
            ) STRICT;", [])?;
            Ok(())
        }).await.unwrap().unwrap();

        EngineState {
            db_pool: pool,
            ast: Arc::new(ast),
        }
    }

    #[tokio::test]
    async fn test_api_scalar_types_and_optionals() {
        let state = build_scalar_test_state().await;
        let app = Router::new().route("/", post(api_execution_handler)).with_state(state.clone());

        // Create a record
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{
                    "model": "Config",
                    "action": "create",
                    "data": {
                        "isPublished": true,
                        "rating": 4.5,
                        "scores": [90, 100, 85]
                    },
                    "select": { "id": true, "isPublished": true, "rating": true, "scores": true, "nickname": true }
                }"#
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json_body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        
        let root = &json_body["data"];
        
        // Assert Boolean correctly formatted as JSON true (not 1)
        assert_eq!(root["isPublished"], true);
        
        // Assert Float is correctly formatted as a number (not string)
        assert_eq!(root["rating"], 4.5);
        
        // Assert Int[] array contains numeric types
        let scores = root["scores"].as_array().unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0], 90);
        assert_eq!(scores[1], 100);
        assert_eq!(scores[2], 85);
        
        // Assert missing optional field safely falls back to explicit JSON null
        assert!(root["nickname"].is_null());
    }
}
