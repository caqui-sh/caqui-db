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

pub fn parse_git_args(args: &[String]) -> Result<Vec<String>, String> {
    let mut subcommand_idx = None;
    let mut skip_next = false;

    // Identify global flags that take arguments to skip over them
    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }

        if arg.starts_with('-') {
            // Check known git global flags that require a separate parameter
            // e.g. git -c key=value, git -C path
            if arg == "-C" 
                || arg == "-c" 
                || arg == "--exec-path" 
                || arg == "--git-dir" 
                || arg == "--work-tree" 
                || arg == "--namespace" 
                || arg == "--super-prefix" 
            {
                skip_next = true;
            }
        } else {
            // First non-flag argument is the subcommand
            subcommand_idx = Some(i);
            break;
        }
    }

    if let Some(idx) = subcommand_idx {
        if args[idx] == "merge" {
            // Check if user tries to override strategy
            for arg in &args[idx + 1..] {
                if arg == "-s" || arg == "--strategy" || arg.starts_with("--strategy=") {
                    return Err("Error: caqui enforces the 'sqlitevfs' merge driver for database integrity. You cannot override it with a custom strategy.".to_string());
                }
            }

            // Inject the sqlitevfs strategy immediately after 'merge'
            let mut pass_args = args.to_vec();
            pass_args.insert(idx + 1, "sqlitevfs".to_string());
            pass_args.insert(idx + 1, "-s".to_string());
            return Ok(pass_args);
        }
    }

    Ok(args.to_vec())
}

pub fn proxy_git_command(args: Vec<String>) {
    let pass_args = match parse_git_args(&args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{}", e);
            exit(1);
        }
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_args_simple_merge() {
        let args = vec!["merge".to_string(), "branch".to_string()];
        let parsed = parse_git_args(&args).unwrap();
        assert_eq!(parsed, vec!["merge", "-s", "sqlitevfs", "branch"]);
    }

    #[test]
    fn test_parse_git_args_with_global_flags() {
        let args = vec![
            "-c".to_string(), 
            "core.editor=vim".to_string(), 
            "-C".to_string(), 
            "/path".to_string(), 
            "merge".to_string(), 
            "branch".to_string()
        ];
        let parsed = parse_git_args(&args).unwrap();
        assert_eq!(parsed, vec!["-c", "core.editor=vim", "-C", "/path", "merge", "-s", "sqlitevfs", "branch"]);
    }

    #[test]
    fn test_parse_git_args_override_error() {
        let args = vec!["merge".to_string(), "-s".to_string(), "recursive".to_string(), "branch".to_string()];
        let err = parse_git_args(&args).unwrap_err();
        assert!(err.contains("You cannot override it with a custom strategy"));
    }

    #[test]
    fn test_parse_git_args_override_error_equals() {
        let args = vec!["merge".to_string(), "--strategy=recursive".to_string(), "branch".to_string()];
        let err = parse_git_args(&args).unwrap_err();
        assert!(err.contains("You cannot override it with a custom strategy"));
    }

    #[test]
    fn test_parse_git_args_non_merge() {
        let args = vec!["commit".to_string(), "-m".to_string(), "merge".to_string()];
        let parsed = parse_git_args(&args).unwrap();
        assert_eq!(parsed, vec!["commit", "-m", "merge"]);
    }
}
