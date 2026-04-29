use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let target = env::var("TARGET").unwrap();

    let asset_name = if target.contains("x86_64-unknown-linux") {
        "git-sqlite-vfs-linux-amd64.zip"
    } else if target.contains("aarch64-unknown-linux") {
        "git-sqlite-vfs-linux-arm64.zip"
    } else if target.contains("aarch64-apple-darwin") {
        "git-sqlite-vfs-darwin-arm64.zip"
    } else {
        println!("cargo:warning=Unsupported target {} for git-sqlite-vfs. Creating a dummy file.", target);
        let dummy_path = PathBuf::from(&out_dir).join("git-merge-sqlitevfs");
        fs::write(&dummy_path, "").unwrap();
        return;
    };

    let download_url = format!(
        "https://github.com/caqui-sh/git-sqlite-vfs/releases/latest/download/{}",
        asset_name
    );

    let zip_path = PathBuf::from(&out_dir).join("release.zip");
    let extract_dir = PathBuf::from(&out_dir).join("extracted");

    // Download the release zip using curl
    let curl_status = Command::new("curl")
        .arg("-sL")
        .arg(&download_url)
        .arg("-o")
        .arg(&zip_path)
        .status()
        .expect("Failed to execute curl");

    if !curl_status.success() {
        println!("cargo:warning=Failed to download git-sqlite-vfs from {}", download_url);
        let dummy_path = PathBuf::from(&out_dir).join("git-merge-sqlitevfs");
        fs::write(&dummy_path, "").unwrap();
        return;
    }

    // Unzip the downloaded file
    fs::create_dir_all(&extract_dir).unwrap();
    let unzip_status = Command::new("unzip")
        .arg("-q")
        .arg("-o")
        .arg(&zip_path)
        .arg("-d")
        .arg(&extract_dir)
        .status()
        .expect("Failed to execute unzip");

    if !unzip_status.success() {
        println!("cargo:warning=Failed to unzip {}", zip_path.display());
        let dummy_path = PathBuf::from(&out_dir).join("git-merge-sqlitevfs");
        fs::write(&dummy_path, "").unwrap();
        return;
    }

    // Locate the extracted git-merge-sqlitevfs binary and move it to OUT_DIR directly
    // The zip might contain the binary at the root or within a folder, so we search for it.
    let mut found = false;
    for entry in fs::read_dir(&extract_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() && path.file_name().unwrap() == "git-merge-sqlitevfs" {
            fs::copy(&path, PathBuf::from(&out_dir).join("git-merge-sqlitevfs")).unwrap();
            found = true;
            break;
        } else if path.is_dir() {
            let inner_path = path.join("git-merge-sqlitevfs");
            if inner_path.exists() && inner_path.is_file() {
                fs::copy(&inner_path, PathBuf::from(&out_dir).join("git-merge-sqlitevfs")).unwrap();
                found = true;
                break;
            }
        }
    }

    if !found {
        println!("cargo:warning=Could not find git-merge-sqlitevfs in the extracted zip archive.");
        let dummy_path = PathBuf::from(&out_dir).join("git-merge-sqlitevfs");
        fs::write(&dummy_path, "").unwrap();
    }
}
