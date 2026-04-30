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
            id String @id
            profileId String
            profile Profile @relation(fields: [profileId], references: [id], deferrable: true)
        }
        
        model Profile {
            id String @id
            userId String
            user User @relation(fields: [userId], references: [id], deferrable: true)
        }
    ";
    fs::write(workspace.join("schema.cq"), schema).unwrap();

    // 3. Push schema
    let mut cmd = Command::new(caqui_bin);
    cmd.args(&["schema", "db-push"]).current_dir(workspace);
    run_cmd(cmd);

    // 4. Create connection pool
    let pool = engine_core::pool::create_pool(&db_uri);
    
    // 5. Test Deferrable - Success within a Transaction
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
    
    // 6. Test Deferrable - Failure if not committed properly (or if single insert without transaction)
    let conn = pool.get().await.unwrap();
    conn.interact(|db| {
        // Trying to insert just the User outside of a transaction where the Profile doesn't exist
        // Should immediately succeed during the INSERT because it's DEFERRED, but fail on the implicit COMMIT
        let result = db.execute("INSERT INTO User (id, profileId) VALUES ('u2', 'p2')", []);
        assert!(result.is_err(), "Insertion should fail because the related profile doesn't exist when the implicit transaction commits");
        
        if let Err(rusqlite::Error::SqliteFailure(err, _)) = result {
            assert_eq!(err.code, rusqlite::ErrorCode::ConstraintViolation, "Error should be a ConstraintViolation");
        } else {
            panic!("Expected ConstraintViolation, got {:?}", result);
        }
        
        Ok::<(), rusqlite::Error>(())
    }).await.unwrap().unwrap();
}