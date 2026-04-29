use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, exit};

// Embed the downloaded binary into our executable
const MERGE_DRIVER_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/git-merge-sqlitevfs"));

fn setup_merge_driver() -> PathBuf {
    let current_dir = env::current_dir().expect("Failed to get current directory");
    let caqui_dir = current_dir.join(".caqui");
    let bin_dir = caqui_dir.join("bin");
    let driver_path = bin_dir.join("git-merge-sqlitevfs");

    if !driver_path.exists() {
        fs::create_dir_all(&bin_dir).expect("Failed to create .caqui/bin directory");
        fs::write(&driver_path, MERGE_DRIVER_BYTES).expect("Failed to write git-merge-sqlitevfs binary");
        
        // Set executable permissions
        let mut perms = fs::metadata(&driver_path).expect("Failed to read metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&driver_path, perms).expect("Failed to set executable permissions");
    }

    bin_dir
}

pub fn proxy_git_command(args: Vec<String>) {
    let bin_dir = setup_merge_driver();

    let mut git_cmd = Command::new("git");

    // Add .caqui/bin to the PATH so Git can discover our custom merge driver
    if let Some(path) = env::var_os("PATH") {
        let mut new_path = bin_dir.into_os_string();
        new_path.push(":");
        new_path.push(path);
        git_cmd.env("PATH", new_path);
    } else {
        git_cmd.env("PATH", bin_dir);
    }

    let mut pass_args = Vec::new();
    let mut is_merge = false;

    if let Some(first_arg) = args.first() {
        if first_arg == "merge" {
            is_merge = true;
        }
    }

    for (i, arg) in args.iter().enumerate() {
        pass_args.push(arg.clone());
        if i == 0 && is_merge {
            pass_args.push("-s".to_string());
            pass_args.push("sqlitevfs".to_string()); // Using 'sqlitevfs' as strategy name to match git-merge-sqlitevfs binary naming convention
        }
    }

    git_cmd.args(&pass_args);

    let mut child = git_cmd.spawn().unwrap_or_else(|e| {
        eprintln!("Failed to execute git: {}", e);
        exit(1);
    });

    let status = child.wait().unwrap_or_else(|e| {
        eprintln!("Failed to wait for git process: {}", e);
        exit(1);
    });

    exit(status.code().unwrap_or(1));
}
