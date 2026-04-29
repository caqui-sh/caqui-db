use engine_core::vfs::{bootstrap_custom_vfs, configure_connection};
use rusqlite::{Connection, OpenFlags};
use tempfile::tempdir;

#[test]
fn test_vfs_single_user_functional() {
    bootstrap_custom_vfs();

    let dir = tempdir().expect("Failed to create temp dir");
    let db_path = dir.path().join("vfs_functional_test.db");

    let mut conn = Connection::open_with_flags_and_vfs(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        "git",
    )
    .expect("Failed to open DB with git VFS");

    configure_connection(&mut conn).expect("Failed to configure connection");

    conn.execute(
        "CREATE TABLE IF NOT EXISTS test_data (id INTEGER PRIMARY KEY, data JSON)",
        [],
    )
    .expect("Failed to create table");

    conn.execute(
        "INSERT INTO test_data (id, data) VALUES (1, '[]')",
        [],
    )
    .expect("Failed to insert initial data");

    // Perform sequential operations to validate functional correctness
    for i in 2..=100 {
        conn.execute(
            "UPDATE test_data SET data = json_insert(data, '$[#]', ?1) WHERE id = 1",
            [i],
        )
        .expect("Failed to update JSON array");
    }

    let count: usize = conn
        .query_row(
            "SELECT json_array_length(data) FROM test_data WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("Failed to query json array length");

    assert_eq!(count, 99, "Incorrect number of writes detected");
    
    // Check if the directory structure expected by git-sqlite-vfs is there
    assert!(db_path.exists());
    assert!(db_path.is_dir());
    assert!(db_path.join("pages").exists());
    }