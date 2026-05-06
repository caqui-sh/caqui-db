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
async fn test_e2e_deferrable() {
    let dir = tempdir().unwrap();
    let workspace = dir.path();
    engine_core::vfs::bootstrap_custom_vfs();
    
    let caqui_bin = env!("CARGO_BIN_EXE_caqui");
    let db_uri = format!("file:{}?vfs=git", workspace.join("app.db").display());

    // 1. Initialize caqui
    let mut cmd = Command::new(caqui_bin);
    cmd.arg("init").current_dir(workspace);
    run_cmd(cmd);

    // 2. Define schema with circular dependency using 
    let schema = r#"
        model User {
            teamId: String
            team: Team @relation("TeamUsers")
            adminTeams: Team[] @relation("TeamAdmin")
            comments: Comment[]
            @@id(uuid)
        }
        
        model Team {
            adminId: String
            admin: User @relation("TeamAdmin")
            users: User[] @relation("TeamUsers")
            @@id(uuid)
        }
        
        model Comment {
            text: String
            userId: String
            user: User 
            @@id(uuid)
        }
    "#;
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    // 3. Push schema
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "push"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Create connection pool
    let pool = api_layer::db::create_pool(&db_uri);
    
    // Debug schema
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let mut stmt = db.prepare("SELECT sql FROM sqlite_schema WHERE type='table'").unwrap();
        let rows: Vec<String> = stmt.query_map([], |row| row.get(0)).unwrap().map(|r| r.unwrap()).collect();
        println!("SCHEMA: {:#?}", rows);
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 5. Test Deferrable - Success within a Transaction (Circular Insert)
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (__id, teamId) VALUES ('u1', 'g1');
            INSERT INTO Team (__id, adminId) VALUES ('g1', 'u1');
            COMMIT;
        ").unwrap();
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 6. Test Scenario 1: Atomic Rollback of Invalid States
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let result = db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (__id, teamId) VALUES ('u2', 'non-existent-group');
            COMMIT;
        ");
        
        assert!(result.is_err(), "Transaction should fail at COMMIT due to constraint violation");
        
        // SQLite doesn't auto-rollback on constraint violations during COMMIT, so we explicitly rollback
        db.execute_batch("ROLLBACK;").unwrap();
        
        // Verify atomic rollback (no partial data)
        let mut stmt = db.prepare("SELECT count(*) FROM User WHERE __id = 'u2'").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "User u2 should have been rolled back completely");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 7. Test Scenario 2: Cyclic Updates (Swap)
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        // Setup a second valid pair
        db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (__id, teamId) VALUES ('u3', 't3');
            INSERT INTO Team (__id, adminId) VALUES ('t3', 'u3');
            COMMIT;
        ").unwrap();
        
        // Swap them!
        db.execute_batch("
            BEGIN TRANSACTION;
            UPDATE User SET teamId = 't3' WHERE __id = 'u1';
            UPDATE User SET teamId = 'g1' WHERE __id = 'u3';
            
            UPDATE Team SET adminId = 'u3' WHERE __id = 'g1';
            UPDATE Team SET adminId = 'u1' WHERE __id = 't3';
            COMMIT;
        ").unwrap();
        
        // Verify Swap Succeeded
        let mut stmt = db.prepare("SELECT teamId FROM User WHERE __id = 'u1'").unwrap();
        let p_id: String = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(p_id, "t3");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 8. Test Scenario 3: Deletion Order Independence
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        // Setup User and Comment
        db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (__id, teamId) VALUES ('u4', 'g4');
            INSERT INTO Team (__id, adminId) VALUES ('g4', 'u4');
            INSERT INTO Comment (__id, text, userId) VALUES ('c4', 'hello', 'u4');
            COMMIT;
        ").unwrap();
        
        // Normally, deleting User before Comment triggers an immediate Restrict error.
        // Because of  we can delete the User first inside a transaction!
        db.execute_batch("
            BEGIN TRANSACTION;
            DELETE FROM User WHERE __id = 'u4';
            DELETE FROM Comment WHERE __id = 'c4';
            DELETE FROM Team WHERE __id = 'g4';
            COMMIT;
        ").unwrap();
        
        // Verify completely gone
        let mut stmt = db.prepare("SELECT count(*) FROM User WHERE __id = 'u4'").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}