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

    // 2. Define schema with circular dependency using deferrable: true
    let schema = "
        model User {
            id: String @id
            profileId: String
            profile: Profile @relation(fields: [profileId], references: [id], deferrable: true)
            comments: Comment[]
        }
        
        model Profile {
            id: String @id
            userId: String
            user: User @relation(fields: [userId], references: [id], deferrable: true)
        }
        
        model Comment {
            id: String @id
            text: String
            userId: String
            user: User @relation(fields: [userId], references: [id], deferrable: true)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    // 3. Push schema
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Create connection pool
    let pool = api_layer::db::create_pool(&db_uri);
    
    // 5. Test Deferrable - Success within a Transaction (Circular Insert)
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (id, profileId) VALUES ('u1', 'p1');
            INSERT INTO Profile (id, userId) VALUES ('p1', 'u1');
            COMMIT;
        ").unwrap();
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 6. Test Scenario 1: Atomic Rollback of Invalid States
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        let result = db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (id, profileId) VALUES ('u2', 'non-existent-profile');
            COMMIT;
        ");
        
        assert!(result.is_err(), "Transaction should fail at COMMIT due to constraint violation");
        
        // SQLite doesn't auto-rollback on constraint violations during COMMIT, so we explicitly rollback
        db.execute_batch("ROLLBACK;").unwrap();
        
        // Verify atomic rollback (no partial data)
        let mut stmt = db.prepare("SELECT count(*) FROM User WHERE id = 'u2'").unwrap();
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
            INSERT INTO User (id, profileId) VALUES ('u3', 'p3');
            INSERT INTO Profile (id, userId) VALUES ('p3', 'u3');
            COMMIT;
        ").unwrap();
        
        // Swap them!
        db.execute_batch("
            BEGIN TRANSACTION;
            UPDATE User SET profileId = 'p3' WHERE id = 'u1';
            UPDATE User SET profileId = 'p1' WHERE id = 'u3';
            
            UPDATE Profile SET userId = 'u3' WHERE id = 'p1';
            UPDATE Profile SET userId = 'u1' WHERE id = 'p3';
            COMMIT;
        ").unwrap();
        
        // Verify Swap Succeeded
        let mut stmt = db.prepare("SELECT profileId FROM User WHERE id = 'u1'").unwrap();
        let p_id: String = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(p_id, "p3");
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
    
    // 8. Test Scenario 3: Deletion Order Independence
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        // Setup User and Comment
        db.execute_batch("
            BEGIN TRANSACTION;
            INSERT INTO User (id, profileId) VALUES ('u4', 'p4');
            INSERT INTO Profile (id, userId) VALUES ('p4', 'u4');
            INSERT INTO Comment (id, text, userId) VALUES ('c4', 'hello', 'u4');
            COMMIT;
        ").unwrap();
        
        // Normally, deleting User before Comment triggers an immediate Restrict error.
        // Because of deferrable: true, we can delete the User first inside a transaction!
        db.execute_batch("
            BEGIN TRANSACTION;
            DELETE FROM User WHERE id = 'u4';
            DELETE FROM Comment WHERE id = 'c4';
            DELETE FROM Profile WHERE id = 'p4';
            COMMIT;
        ").unwrap();
        
        // Verify completely gone
        let mut stmt = db.prepare("SELECT count(*) FROM User WHERE id = 'u4'").unwrap();
        let count: i64 = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}