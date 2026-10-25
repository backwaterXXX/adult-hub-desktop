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
    let path = remote[host..].trim_start_matches([':', '/']);
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

    let commit_hash = std::env::var("ADULT_HUB_COMMIT_HASH").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "--short=8", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into())
    });

    let build_date = std::env::var("ADULT_HUB_BUILD_DATE").unwrap_or_else(|_| {
        Command::new("date")
            .args(["-u", "+%Y-%m-%d"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "2026-10-21".into())
    });

    let dirty_output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
        .unwrap_or_default();
    let dirty = !String::from_utf8_lossy(&dirty_output).trim().is_empty();

    let dirty_suffix = if dirty {
        let diff_output = Command::new("git")
            .args(["diff", "HEAD"])
            .output()
            .ok()
            .map(|o| o.stdout)
            .unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::Hasher;
        hasher.write(&diff_output);
        hasher.write(&dirty_output);
        let h = hasher.finish();
        format!("-dirty.{:08x}", (h & 0xffff_ffff) as u32)
    } else {
        String::new()
    };

    println!("cargo:rerun-if-env-changed=ADULT_HUB_COMMIT_HASH");
    println!("cargo:rerun-if-env-changed=ADULT_HUB_BUILD_DATE");
    println!("cargo:rerun-if-env-changed=ADULT_HUB_BUILD_ID");
    println!("cargo:rustc-env=ADULT_HUB_COMMIT_HASH={commit_hash}");
    println!("cargo:rustc-env=ADULT_HUB_BUILD_DATE={build_date}");
    println!(
        "cargo:rustc-env=ADULT_HUB_DIRTY={}",
        if dirty { "1" } else { "0" }
    );
    let build_id = std::env::var("ADULT_HUB_BUILD_ID").unwrap_or_else(|_| {
        format!(
            "{}-{}{}",
            std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()),
            commit_hash,
            dirty_suffix
        )
    });
    println!("cargo:rustc-env=ADULT_HUB_BUILD_ID={build_id}");

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
        let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
        let version_parts: Vec<&str> = pkg_version.split('.').collect();
        let major: u16 = version_parts
            .first()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let minor: u16 = version_parts
            .get(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let patch: u16 = version_parts
            .get(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let rc_content = format!(
            r#"1 ICON "{icon_path}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Adult Hub\0"
            VALUE "FileDescription", "Adult Hub Desktop\0"
            VALUE "FileVersion", "{pkg_version}.0\0"
            VALUE "InternalName", "adult-hub-desktop\0"
            VALUE "LegalCopyright", "Copyright (C) 2026 Adult Hub\0"
            VALUE "OriginalFilename", "adult-hub-desktop.exe\0"
            VALUE "ProductName", "Adult Hub Desktop\0"
            VALUE "ProductVersion", "{pkg_version}\0"
            VALUE "SpecialBuild", "{build_id}\0"
            VALUE "Comments", "Build ID: {build_id}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
        );
        fs::write(&resource_script, rc_content).expect("failed to write Windows resource script");

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
