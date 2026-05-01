use std::process::Command;
use std::env;
use std::fs;
use tempfile::tempdir;
use serde_json::json;

fn run_cmd(mut cmd: Command) -> String {
    let output = cmd.output().unwrap_or_else(|e| panic!("Failed to execute process: {:?}", e));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        panic!("Command failed: {:?}\nSTDOUT:\n{}\nSTDERR:\n{}", 
               cmd, 
               stdout,
               stderr);
    }
    stdout
}

#[tokio::test]
async fn test_e2e_relation_names() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // 2. Define schema with multiple relations
    let schema = "
        model User {
            id: String @id
            name: String
            authoredPosts: Post[] @relation(\"AuthorToPost\")
            reviewedPosts: Post[] @relation(\"ReviewerToPost\")
        }
        
        model Post {
            id: String @id
            title: String
            authorId: String
            author: User @relation(\"AuthorToPost\", fields: [authorId], references: [id])
            reviewerId: String
            reviewer: User @relation(\"ReviewerToPost\", fields: [reviewerId], references: [id])
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    // 3. Push schema
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Create connection pool & Insert Data
    let pool = api_layer::db::create_pool(&db_uri);
    
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (id, name) VALUES ('u1', 'Alice');
            INSERT INTO User (id, name) VALUES ('u2', 'Bob');
            
            -- Alice authors Post 1, Bob reviews it
            INSERT INTO Post (id, title, authorId, reviewerId) VALUES ('p1', 'Rust Guide', 'u1', 'u2');
            
            -- Bob authors Post 2, Alice reviews it
            INSERT INTO Post (id, title, authorId, reviewerId) VALUES ('p2', 'SQLite Tips', 'u2', 'u1');
            
            -- Alice authors Post 3, no reviewer
            INSERT INTO Post (id, title, authorId, reviewerId) VALUES ('p3', 'Zero Overhead', 'u1', NULL);
            COMMIT;
        ").unwrap();
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();

    // 5. Build State & Execute API Payload directly via API core (bypassing HTTP router for E2E speed)
    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    
    let payload = json!({
        "action": "findMany",
        "select": {
            "name": true,
            "authoredPosts": {
                "select": { "title": true }
            },
            "reviewedPosts": {
                "select": { "title": true }
            }
        },
        "where": {
            "name": "Alice"
        }
    });

    let mut alias_counter = 0;
    let ir = api_layer::translator::hydrate_payload_to_ir(&ast, "User", &payload, &mut alias_counter).unwrap();
    let sql = query_compiler::read::compile_select(&ir, None);

    let conn2 = pool.get().await.unwrap();
    let result_json: String = conn2.interact(move |db| {
        let mut stmt = db.prepare(&sql).unwrap();
        stmt.query_row([], |row| row.get(0))
    }).await.unwrap().unwrap();

    let result_val: serde_json::Value = serde_json::from_str(&result_json).unwrap();
    let rows = result_val.as_array().unwrap();
    
    assert_eq!(rows.len(), 1, "Should return exactly one user (Alice)");
    let alice = &rows[0];
    assert_eq!(alice["name"], "Alice");
    
    // Assert Authored Posts resolved correctly (AuthorToPost -> authorId)
    let authored = alice["authoredPosts"].as_array().unwrap();
    assert_eq!(authored.len(), 2, "Alice authored 2 posts");
    let titles: Vec<_> = authored.iter().map(|p| p["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Rust Guide"));
    assert!(titles.contains(&"Zero Overhead"));
    
    // Assert Reviewed Posts resolved correctly (ReviewerToPost -> reviewerId)
    let reviewed = alice["reviewedPosts"].as_array().unwrap();
    assert_eq!(reviewed.len(), 1, "Alice reviewed 1 post");
    assert_eq!(reviewed[0]["title"], "SQLite Tips");
}

#[tokio::test]
async fn test_e2e_self_referential_relations() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        model Employee {
            id: String @id
            name: String
            managerId: String
            manager: Employee @relation(\"ManagerToEmployee\", fields: [managerId], references: [id])
            directReports: Employee[] @relation(\"ManagerToEmployee\")
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(cmd);

    let pool = api_layer::db::create_pool(&db_uri);
    
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO Employee (id, name, managerId) VALUES ('e1', 'CEO', 'e1');
            INSERT INTO Employee (id, name, managerId) VALUES ('e2', 'VP', 'e1');
            INSERT INTO Employee (id, name, managerId) VALUES ('e3', 'Manager', 'e2');
            INSERT INTO Employee (id, name, managerId) VALUES ('e4', 'IC', 'e3');
            COMMIT;
        ").unwrap();
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    
    let payload = json!({
        "action": "findMany",
        "select": {
            "name": true,
            "directReports": {
                "select": {
                    "name": true,
                    "directReports": {
                        "select": {
                            "name": true,
                            "manager": {
                                "select": { "name": true }
                            }
                        }
                    }
                },
                "where": {
                    "name": { "notEq": "CEO" }
                }
            }
        },
        "where": {
            "name": "CEO"
        }
    });

    let mut alias_counter = 0;
    let ir = api_layer::translator::hydrate_payload_to_ir(&ast, "Employee", &payload, &mut alias_counter).unwrap();
    let sql = query_compiler::read::compile_select(&ir, None);
    println!("GENERATED SQL:\n{}", sql);

    let conn2 = pool.get().await.unwrap();
    let result_json: String = conn2.interact(move |db| {
        let mut stmt = db.prepare(&sql).unwrap();
        stmt.query_row([], |row| row.get(0))
    }).await.unwrap().unwrap();

    let result_val: serde_json::Value = serde_json::from_str(&result_json).unwrap();
    let rows = result_val.as_array().unwrap();
    
    assert_eq!(rows.len(), 1, "Should return exactly one CEO");
    let ceo = &rows[0];
    assert_eq!(ceo["name"], "CEO");
    
    let ceo_directs = ceo["directReports"].as_array().unwrap();
    assert_eq!(ceo_directs.len(), 1, "CEO has 1 real direct report (VP)");
    assert_eq!(ceo_directs[0]["name"], "VP");
    
    let vp_directs = ceo_directs[0]["directReports"].as_array().unwrap();
    assert_eq!(vp_directs.len(), 1, "VP has 1 direct report (Manager)");
    assert_eq!(vp_directs[0]["name"], "Manager");
    
    let manager_manager = &vp_directs[0]["manager"];
    assert_eq!(manager_manager["name"], "VP", "Manager's manager is VP");
}