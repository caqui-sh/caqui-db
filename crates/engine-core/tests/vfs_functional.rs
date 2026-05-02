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
        "CREATE TABLE IF NOT EXISTS test_data (__id INTEGER PRIMARY KEY, data JSON)",
        [],
    )
    .expect("Failed to create table");

    conn.execute(
        "INSERT INTO test_data (__id, data) VALUES (1, '[]')",
        [],
    )
    .expect("Failed to insert initial data");

    // Perform sequential operations to validate functional correctness
    for i in 2..=100 {
        conn.execute(
            "UPDATE test_data SET data = json_insert(data, '$[#]', ?1) WHERE __id = 1",
            [i],
        )
        .expect("Failed to update JSON array");
    }

    let count: usize = conn
        .query_row(
            "SELECT json_array_length(data) FROM test_data WHERE __id = 1",
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

#[test]
fn test_vfs_corrupted_file_graceful_error() {
    bootstrap_custom_vfs();
    
    let dir = tempdir().expect("Failed to create temp dir");
    let corrupted_path = dir.path().join("corrupted.db");
    
    // Create a directory structure that looks like a git-sqlite-vfs database
    std::fs::create_dir_all(corrupted_path.join("pages")).unwrap();
    // Write exactly 4096 bytes of random garbage into the first page (SQLite header)
    let garbage = vec![0xFF; 4096];
    std::fs::write(corrupted_path.join("pages").join("00000000.page"), garbage).unwrap();
    
    let conn = Connection::open_with_flags_and_vfs(
        &corrupted_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE,
        "git",
    ).expect("VFS opens paths unconditionally");
    
    // Attempting to read from a corrupted/invalid DB using the Git VFS 
    // should gracefully handle the bad pages (e.g. by considering the DB empty) 
    // and not crash the process.
    let result = conn.execute("SELECT 1 FROM sqlite_master", []);
    
    assert!(result.is_ok(), "Querying corrupted DB should gracefully recover or return empty without panicking");
}