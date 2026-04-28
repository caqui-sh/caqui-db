use rusqlite::ffi;
use std::os::raw::{c_char, c_int};
use std::sync::OnceLock;

static VFS_INIT: OnceLock<()> = OnceLock::new();

pub fn bootstrap_custom_vfs() {
    VFS_INIT.get_or_init(|| unsafe {
        unsafe extern "C" {
            fn sqlite3_gitvfs_init_impl(base_dir: *const c_char) -> c_int;
        }

        let rc = sqlite3_gitvfs_init_impl(std::ptr::null());
        assert_eq!(rc, ffi::SQLITE_OK, "FATAL: Failed to bootstrap git-sqlite-vfs.");

        // Find the VFS by name "git"
        let vfs_name = c"git".as_ptr();
        let vfs_ptr = ffi::sqlite3_vfs_find(vfs_name);
        assert!(!vfs_ptr.is_null(), "FATAL: git-sqlite-vfs not found in SQLite registry.");

        // Register the VFS but DO NOT set it as the default (0)
        ffi::sqlite3_vfs_register(vfs_ptr, 0);
    });
}

pub fn configure_connection(conn: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
    // 1. Enforce Write-Ahead Logging for concurrency
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // 2. Safe in WAL, maximizes disk I/O speed
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    // 3. Keep temp tables/indexes in RAM
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    // 4. Handle OS-level lock contention gracefully
    conn.pragma_update(None, "busy_timeout", "5000")?;
    Ok(())
}