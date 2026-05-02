use std::process::Command;
use std::env;
use std::fs;
use tempfile::tempdir;

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
async fn test_e2e_on_delete() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // 2. Define schema
    let schema = "
        model User {

            name: String
            posts: Post[]
            profiles: Profile[]
            comments: Comment[]
    @@id(uuid)
        }
        
        model Post {

            title: String
            userId: String
            user: User @relation(fields: [userId], references: [__id], onDelete: Cascade)
    @@id(uuid)
        }
        
        model Profile {

            bio: String
            userId: String
            user: User @relation(fields: [userId], references: [__id], onDelete: SetNull)
    @@id(uuid)
        }
        
        model Comment {

            text: String
            userId: String
            user: User @relation(fields: [userId], references: [__id], onDelete: Restrict)
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    // 3. Push schema
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Create a connection pool to test PRAGMA foreign_keys = ON
    let pool = api_layer::db::create_pool(&db_uri);
    
    // 5. Test Cascade
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name) VALUES ('u1', 'Alice')", []).unwrap();
        db.execute("INSERT INTO Post (__id, title, userId) VALUES ('p1', 'Post 1', 'u1')", []).unwrap();
        
        // Delete user
        db.execute("DELETE FROM User WHERE __id = 'u1'", []).unwrap();
        
        // Assert post is gone
        let mut stmt = db.prepare("SELECT count(*) FROM Post WHERE __id = 'p1'").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "Post should have been cascaded");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 6. Test SetNull
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name) VALUES ('u2', 'Bob')", []).unwrap();
        db.execute("INSERT INTO Profile (__id, bio, userId) VALUES ('pr1', 'Bio 1', 'u2')", []).unwrap();
        
        // Delete user
        db.execute("DELETE FROM User WHERE __id = 'u2'", []).unwrap();
        
        // Assert profile exists but userId is NULL
        let mut stmt = db.prepare("SELECT userId FROM Profile WHERE __id = 'pr1'").unwrap();
        let user_id: Option<String> = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(user_id, None, "Profile userId should have been set to NULL");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 7. Test Restrict
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (__id, name) VALUES ('u3', 'Charlie')", []).unwrap();
        db.execute("INSERT INTO Comment (__id, text, userId) VALUES ('c1', 'Comment 1', 'u3')", []).unwrap();
        
        // Attempt to delete user
        let result = db.execute("DELETE FROM User WHERE __id = 'u3'", []);
        assert!(result.is_err(), "Deletion should have been restricted");
        
        if let Err(rusqlite::Error::SqliteFailure(err, _)) = result {
            assert_eq!(err.code, rusqlite::ErrorCode::ConstraintViolation, "Error should be a ConstraintViolation");
        } else {
            panic!("Expected ConstraintViolation, got {:?}", result);
        }
        
        // Assert user still exists
        let mut stmt = db.prepare("SELECT count(*) FROM User WHERE __id = 'u3'").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1, "User should still exist");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}

#[tokio::test]
async fn test_e2e_self_referential_cascade() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    // Re-initialize VFS isn't strictly necessary per-test, but safe
    let _ = engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        model Employee {

            name: String
            managerId: String?
            manager: Employee? @relation(\"Management\", fields: [managerId], references: [__id], onDelete: Cascade)
            subordinates: Employee[] @relation(\"Management\")
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let pool = api_layer::db::create_pool(&db_uri);
    
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        // CEO
        db.execute("INSERT INTO Employee (__id, name) VALUES ('ceo', 'CEO')", []).unwrap();
        // Manager (reports to CEO)
        db.execute("INSERT INTO Employee (__id, name, managerId) VALUES ('mgr', 'Manager', 'ceo')", []).unwrap();
        // Intern (reports to Manager)
        db.execute("INSERT INTO Employee (__id, name, managerId) VALUES ('intern', 'Intern', 'mgr')", []).unwrap();
        // Sibling Manager (reports to CEO)
        db.execute("INSERT INTO Employee (__id, name, managerId) VALUES ('mgr2', 'Manager 2', 'ceo')", []).unwrap();
        
        // Delete Manager 1
        db.execute("DELETE FROM Employee WHERE __id = 'mgr'", []).unwrap();
        
        // Assert Intern was cascaded
        let mut stmt = db.prepare("SELECT count(*) FROM Employee WHERE __id = 'intern'").unwrap();
        let intern_count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(intern_count, 0, "Intern should have been cascaded");
        
        // Assert Manager 2 is untouched
        let mut stmt = db.prepare("SELECT count(*) FROM Employee WHERE __id = 'mgr2'").unwrap();
        let mgr2_count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(mgr2_count, 1, "Manager 2 should be untouched");
        
        // Delete CEO
        db.execute("DELETE FROM Employee WHERE __id = 'ceo'", []).unwrap();
        
        // Assert Manager 2 is cascaded
        let mut stmt = db.prepare("SELECT count(*) FROM Employee WHERE __id = 'mgr2'").unwrap();
        let mgr2_count_after: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(mgr2_count_after, 0, "Manager 2 should have been cascaded when CEO was deleted");
        
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}

#[tokio::test]
async fn test_e2e_polymorphic_cascade_delete() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        base Content { }
        model Article extends Content { title: String @@id(uuid) }
        model Video extends Content { duration: Int @@id(uuid) }
        
        model Comment {

            text: String
            parent: Content
    @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let payload = serde_json::json!({
        "data": {
            "__id": "c1",
            "text": "Great article!",
            "parent": {
                "Article": { "create": { "__id": "a1", "title": "Polymorphic Writes" } }
            }
        }
    });

    let ast = schema_parser::parser::parse_schema(schema).unwrap();
    let ast = schema_parser::validation::validate_schema(ast).unwrap();
    let pool = api_layer::db::create_pool(&db_uri);
    
    let mut alias_idx = 0;
    let plan = api_layer::mutation_translator::hydrate_mutation_to_plan(&ast, "Comment", "create", &payload, &mut alias_idx).unwrap();
    api_layer::executor::execute_mutation_plan(&pool, plan).await.unwrap();

    let conn = pool.get().await.unwrap();

    let count_before: i64 = conn.interact(|db| {
        db.query_row("SELECT count(*) FROM Comment", [], |r| r.get(0))
    }).await.unwrap().unwrap();
    
    assert_eq!(count_before, 1, "Comment should exist");

    let article_id: String = conn.interact(|db| {
        db.query_row("SELECT __id FROM Article LIMIT 1", [], |r| r.get(0))
    }).await.unwrap().unwrap();
    
    println!("ARTICLE ID: {}", article_id);

    // Delete the Article
    let delete_payload = serde_json::json!({
        "where": {
            "__id": article_id
        }
    });
    
    let mut alias_idx = 0;
    let del_plan = api_layer::mutation_translator::hydrate_mutation_to_plan(&ast, "Article", "delete", &delete_payload, &mut alias_idx).unwrap();
    println!("DELETE PLAN: {:#?}", del_plan);
    api_layer::executor::execute_mutation_plan(&pool, del_plan).await.unwrap();

    // Verify Application-Level Cascade cleaned up the Comment
    let count_after: i64 = conn.interact(|db| {
        db.query_row("SELECT count(*) FROM Comment", [], |r| r.get(0))
    }).await.unwrap().unwrap();
    
    assert_eq!(count_after, 0, "Comment should be deleted by application-level cascade");
}

#[tokio::test]
async fn test_e2e_on_delete_no_action() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    let _ = engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    let schema = "
        model Parent {
            @@id(uuid)
        }
        
        model Child {
            parentId: String
            parent: Parent @relation(fields: [parentId], references: [__id], onDelete: NoAction)
            @@id(uuid)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    let pool = api_layer::db::create_pool(&db_uri);
    let conn = pool.get().await.unwrap();
    
    conn.interact(|db| {
        db.execute("INSERT INTO Parent (__id) VALUES ('p1')", []).unwrap();
        db.execute("INSERT INTO Child (__id, parentId) VALUES ('c1', 'p1')", []).unwrap();
        
        // Delete parent - should fail due to NO ACTION (which behaves like RESTRICT in SQLite when foreign_keys=ON)
        let res = db.execute("DELETE FROM Parent WHERE __id = 'p1'", []);
        assert!(res.is_err(), "Deletion should have been blocked by NO ACTION");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}