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
            id String @id
            name String
            posts Post[]
            profiles Profile[]
            comments Comment[]
        }
        
        model Post {
            id String @id
            title String
            userId String
            user User @relation(fields: [userId], references: [id], onDelete: Cascade)
        }
        
        model Profile {
            id String @id
            bio String
            userId String
            user User @relation(fields: [userId], references: [id], onDelete: SetNull)
        }
        
        model Comment {
            id String @id
            text String
            userId String
            user User @relation(fields: [userId], references: [id], onDelete: Restrict)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    // 3. Push schema
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Create a connection pool to test PRAGMA foreign_keys = ON
    let pool = engine_core::pool::create_pool(&db_uri);
    
    // 5. Test Cascade
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (id, name) VALUES ('u1', 'Alice')", []).unwrap();
        db.execute("INSERT INTO Post (id, title, userId) VALUES ('p1', 'Post 1', 'u1')", []).unwrap();
        
        // Delete user
        db.execute("DELETE FROM User WHERE id = 'u1'", []).unwrap();
        
        // Assert post is gone
        let mut stmt = db.prepare("SELECT count(*) FROM Post WHERE id = 'p1'").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "Post should have been cascaded");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 6. Test SetNull
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (id, name) VALUES ('u2', 'Bob')", []).unwrap();
        db.execute("INSERT INTO Profile (id, bio, userId) VALUES ('pr1', 'Bio 1', 'u2')", []).unwrap();
        
        // Delete user
        db.execute("DELETE FROM User WHERE id = 'u2'", []).unwrap();
        
        // Assert profile exists but userId is NULL
        let mut stmt = db.prepare("SELECT userId FROM Profile WHERE id = 'pr1'").unwrap();
        let user_id: Option<String> = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(user_id, None, "Profile userId should have been set to NULL");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 7. Test Restrict
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute("INSERT INTO User (id, name) VALUES ('u3', 'Charlie')", []).unwrap();
        db.execute("INSERT INTO Comment (id, text, userId) VALUES ('c1', 'Comment 1', 'u3')", []).unwrap();
        
        // Attempt to delete user
        let result = db.execute("DELETE FROM User WHERE id = 'u3'", []);
        assert!(result.is_err(), "Deletion should have been restricted");
        
        if let Err(rusqlite::Error::SqliteFailure(err, _)) = result {
            assert_eq!(err.code, rusqlite::ErrorCode::ConstraintViolation, "Error should be a ConstraintViolation");
        } else {
            panic!("Expected ConstraintViolation, got {:?}", result);
        }
        
        // Assert user still exists
        let mut stmt = db.prepare("SELECT count(*) FROM User WHERE id = 'u3'").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 1, "User should still exist");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}