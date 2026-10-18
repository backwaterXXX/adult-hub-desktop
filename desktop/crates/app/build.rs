use std::process::Command;
use std::{fs, path::PathBuf};

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let (Some(owner), Some(repository), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !owner.is_empty()
        && !repository.is_empty()
        && owner
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        && repository
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

fn repository_from_remote_url(remote: &str) -> Option<String> {
    let host = remote.find("github.com")? + "github.com".len();
    let path = remote[host..].trim_start_matches(|character| character == ':' || character == '/');
    let repository = path.trim_end_matches(".git").trim_end_matches('/');
    valid_repository(repository).then(|| repository.to_owned())
}

fn release_repository() -> String {
    if let Ok(repository) = std::env::var("GITHUB_REPOSITORY") {
        if valid_repository(&repository) {
            return repository;
        }
    }

    Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|remote| repository_from_remote_url(remote.trim()))
        .unwrap_or_else(|| "adult-hub/adult-hub-desktop".to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_REPOSITORY");
    println!(
        "cargo:rustc-env=ADULT_HUB_UPDATE_REPOSITORY={}",
        release_repository()
    );
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=../../assets/adult-hub-desktop.ico");
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");

    // Embed the Windows application icon in the executable as well as setting
    // the runtime window icon from the same asset in main.rs.
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") {
        let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let icon = manifest_dir.join("../../assets/adult-hub-desktop.ico");
        let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        let resource_script = out_dir.join("adult-hub-icon.rc");
        let resource_object = out_dir.join("adult-hub-icon.o");
        let icon_path = icon.to_string_lossy().replace('\\', "/");
        fs::write(&resource_script, format!("1 ICON \"{icon_path}\"\n"))
            .expect("failed to write Windows icon resource script");

        let compiler = if target.ends_with("windows-gnu") {
            std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into())
        } else {
            std::env::var("RC").unwrap_or_else(|_| "rc".into())
        };
        let status = if target.ends_with("windows-gnu") {
            Command::new(&compiler)
                .args(["--target=pe-x86-64", "-O", "coff"])
                .arg(&resource_script)
                .arg("-o")
                .arg(&resource_object)
                .status()
        } else {
            Command::new(&compiler)
                .arg(format!("/fo{}", resource_object.display()))
                .arg(&resource_script)
                .status()
        }
        .unwrap_or_else(|error| {
            panic!("failed to run Windows resource compiler {compiler}: {error}")
        });
        assert!(status.success(), "Windows icon resource compilation failed");
        println!(
            "cargo:rustc-link-arg-bin=adult-hub-desktop={}",
            resource_object.display()
        );
    }
}
