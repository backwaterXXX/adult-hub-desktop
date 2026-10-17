//! In-app update check, manifest authentication, and package verification.
//!
//! Enforces:
//! - Desktop-specific signed manifest (schema version 1)
//! - Detached Ed25519 signature verification against embedded public key
//! - HTTPS-only downloads from configured release hosts
//! - SHA-256 digest and package size verification in a staging file before install
//! - Platform package handoff: Windows NSIS installer, Linux .deb or AppImage

use ring::digest::{self, SHA256};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Embedded release verification Ed25519 public key (32 bytes).
pub const EMBEDDED_RELEASE_PUBLIC_KEY: [u8; 32] = [
    0xc7, 0x20, 0x1f, 0xeb, 0xbc, 0x61, 0x25, 0x0b,
    0x56, 0x27, 0x33, 0x78, 0x68, 0xea, 0xa3, 0xce,
    0xbd, 0x36, 0x43, 0x9d, 0xa7, 0x27, 0x57, 0x39,
    0xa4, 0x4f, 0x7f, 0xfc, 0xf0, 0x7a, 0x8e, 0x43,
];

/// Default allowed release hosts for HTTPS downloads.
pub const DEFAULT_ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "release.adult.ferges.eu",
];

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("unsupported manifest schema version: {0} (expected 1)")]
    InvalidSchemaVersion(u32),

    #[error("manifest signature verification failed")]
    InvalidSignature,

    #[error("insecure update URL: {0} (only HTTPS is permitted)")]
    InsecureUrl(String),

    #[error("update URL host not in allowed release hosts: {0}")]
    DisallowedHost(String),

    #[allow(dead_code)]
    #[error("no matching package found for platform '{0}' and architecture '{1}'")]
    NoMatchingPackage(String, String),

    #[error("SHA-256 hash mismatch: expected {expected}, computed {actual}")]
    HashMismatch { expected: String, actual: String },

    #[error("file size mismatch: expected {expected} bytes, got {actual} bytes")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("download failed: {0}")]
    DownloadFailed(String),

    #[error("I/O error during update: {0}")]
    Io(String),

    #[error("manifest JSON parse error: {0}")]
    Json(String),

    #[error("cannot install update while playback is active")]
    PlaybackActive,

    #[error("AppImage installation path is read-only: {0}")]
    AppImageReadOnly(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub version: String,
    pub release_notes_url: String,
    #[serde(default)]
    pub release_notes: Option<String>,
    pub packages: Vec<UpdatePackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdatePackage {
    pub platform: String,      // "windows" or "linux"
    pub arch: String,          // "x86_64"
    pub package_type: String,  // "installer", "zip", "deb", "appimage"
    pub url: String,           // HTTPS download URL
    pub sha256: String,        // 64-character lowercase hex string
    pub size_bytes: u64,
}

/// Helper to decode signature bytes from either raw binary (64 bytes), hex string, or base64.
pub fn decode_signature_bytes(input: &[u8]) -> Option<Vec<u8>> {
    let trimmed = match std::str::from_utf8(input) {
        Ok(s) => s.trim(),
        Err(_) => {
            if input.len() == 64 {
                return Some(input.to_vec());
            }
            return None;
        }
    };

    // If exact 64 bytes raw
    if input.len() == 64 && !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(input.to_vec());
    }

    // Try hex decode (128 hex characters = 64 bytes)
    if trimmed.len() == 128 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut bytes = Vec::with_capacity(64);
        for i in (0..128).step_by(2) {
            if let Ok(byte) = u8::from_str_radix(&trimmed[i..i + 2], 16) {
                bytes.push(byte);
            } else {
                return None;
            }
        }
        return Some(bytes);
    }

    // Try standard base64 decode if not hex
    if let Ok(decoded) = simple_base64_decode(trimmed) {
        if decoded.len() == 64 {
            return Some(decoded);
        }
    }

    if input.len() == 64 {
        Some(input.to_vec())
    } else {
        None
    }
}

fn simple_base64_decode(input: &str) -> Result<Vec<u8>, ()> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut clean = input.replace(['\r', '\n', ' '], "");
    while clean.ends_with('=') {
        clean.pop();
    }
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for &b in clean.as_bytes() {
        let val = match TABLE.iter().position(|&x| x == b) {
            Some(idx) => idx as u32,
            None => return Err(()),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// Verify an Ed25519 signature over manifest bytes.
pub fn verify_signature(manifest_bytes: &[u8], signature_bytes: &[u8], public_key_bytes: &[u8]) -> Result<(), UpdateError> {
    let raw_sig = decode_signature_bytes(signature_bytes)
        .ok_or(UpdateError::InvalidSignature)?;

    if raw_sig.len() != 64 || public_key_bytes.len() != 32 {
        return Err(UpdateError::InvalidSignature);
    }

    let peer_public_key = UnparsedPublicKey::new(&ED25519, public_key_bytes);
    peer_public_key
        .verify(manifest_bytes, &raw_sig)
        .map_err(|_| UpdateError::InvalidSignature)
}

/// Validate manifest URLs and hosts.
pub fn validate_package_urls(manifest: &UpdateManifest, allowed_hosts: &[&str]) -> Result<(), UpdateError> {
    // Release notes URL must be HTTPS
    if !manifest.release_notes_url.starts_with("https://") {
        return Err(UpdateError::InsecureUrl(manifest.release_notes_url.clone()));
    }

    for package in &manifest.packages {
        if !package.url.starts_with("https://") {
            return Err(UpdateError::InsecureUrl(package.url.clone()));
        }

        let parsed_url = url::Url::parse(&package.url)
            .map_err(|e| UpdateError::InsecureUrl(format!("{}: {e}", package.url)))?;

        let host = parsed_url.host_str().unwrap_or("");
        let is_allowed = allowed_hosts.iter().any(|allowed| {
            host == *allowed || host.ends_with(&format!(".{allowed}"))
        });

        if !is_allowed {
            return Err(UpdateError::DisallowedHost(host.to_string()));
        }

        if package.sha256.len() != 64 || !package.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(UpdateError::HashMismatch {
                expected: "64-char hex SHA-256".into(),
                actual: package.sha256.clone(),
            });
        }
    }

    Ok(())
}

/// Parse, verify detached Ed25519 signature, and validate an update manifest.
pub fn parse_and_verify_manifest(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    public_key_bytes: &[u8],
    allowed_hosts: &[&str],
) -> Result<UpdateManifest, UpdateError> {
    // 1. Authenticate signature first
    verify_signature(manifest_bytes, signature_bytes, public_key_bytes)?;

    // 2. Parse JSON
    let manifest: UpdateManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|e| UpdateError::Json(e.to_string()))?;

    // 3. Check schema version
    if manifest.schema_version != 1 {
        return Err(UpdateError::InvalidSchemaVersion(manifest.schema_version));
    }

    // 4. Validate URLs and hosts
    validate_package_urls(&manifest, allowed_hosts)?;

    Ok(manifest)
}

/// Determine whether `candidate` is strictly newer than `current`.
pub fn is_newer_version(candidate: &str, current: &str) -> bool {
    let parse_ver = |v: &str| -> Vec<u64> {
        let clean = v.trim().trim_start_matches(['v', 'V']);
        clean
            .split(['.', '-', '+'])
            .filter_map(|part| part.parse::<u64>().ok())
            .collect()
    };

    let cand_parts = parse_ver(candidate);
    let curr_parts = parse_ver(current);

    let max_len = cand_parts.len().max(curr_parts.len());
    for i in 0..max_len {
        let c = cand_parts.get(i).copied().unwrap_or(0);
        let cur = curr_parts.get(i).copied().unwrap_or(0);
        if c > cur {
            return true;
        } else if c < cur {
            return false;
        }
    }

    false
}

/// Select best package for current platform and environment.
pub fn select_best_package<'a>(
    packages: &'a [UpdatePackage],
    target_os: &str,
    target_arch: &str,
    is_appimage: bool,
) -> Option<&'a UpdatePackage> {
    let matching: Vec<&'a UpdatePackage> = packages
        .iter()
        .filter(|p| p.platform.eq_ignore_ascii_case(target_os) && p.arch.eq_ignore_ascii_case(target_arch))
        .collect();

    if target_os == "windows" {
        matching
            .iter()
            .find(|p| p.package_type == "installer")
            .copied()
            .or_else(|| matching.iter().find(|p| p.package_type == "zip").copied())
    } else if target_os == "linux" {
        if is_appimage {
            matching
                .iter()
                .find(|p| p.package_type == "appimage")
                .copied()
                .or_else(|| matching.iter().find(|p| p.package_type == "deb").copied())
        } else {
            matching
                .iter()
                .find(|p| p.package_type == "deb")
                .copied()
                .or_else(|| matching.iter().find(|p| p.package_type == "appimage").copied())
        }
    } else {
        matching.first().copied()
    }
}

/// Check file size and SHA-256 integrity.
pub fn verify_file_integrity(file_path: &Path, expected_sha256: &str, expected_size: u64) -> Result<(), UpdateError> {
    let mut file = File::open(file_path).map_err(|e| UpdateError::Io(e.to_string()))?;
    let metadata = file.metadata().map_err(|e| UpdateError::Io(e.to_string()))?;
    if metadata.len() != expected_size {
        return Err(UpdateError::SizeMismatch {
            expected: expected_size,
            actual: metadata.len(),
        });
    }

    let mut context = digest::Context::new(&SHA256);
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| UpdateError::Io(e.to_string()))?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
    }

    let digest_result = context.finish();
    let computed_hex = hex_string(digest_result.as_ref());

    if !computed_hex.eq_ignore_ascii_case(expected_sha256) {
        return Err(UpdateError::HashMismatch {
            expected: expected_sha256.to_string(),
            actual: computed_hex,
        });
    }

    Ok(())
}

fn hex_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Download package into a temporary staging file and verify SHA-256 & size before returning verified path.
pub fn download_and_verify_package(
    package: &UpdatePackage,
    progress_callback: impl Fn(u64, u64),
) -> Result<PathBuf, UpdateError> {
    let temp_dir = std::env::temp_dir();
    let file_stem = format!("adult-hub-update-{}", package.package_type);
    let staging_path = temp_dir.join(format!("{file_stem}.staging"));
    let final_ext = match package.package_type.as_str() {
        "installer" => "exe",
        "deb" => "deb",
        "appimage" => "AppImage",
        "zip" => "zip",
        _ => "bin",
    };
    let verified_path = temp_dir.join(format!("{file_stem}.{final_ext}"));

    // Cleanup previous staging if any
    let _ = std::fs::remove_file(&staging_path);

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            // Ensure redirects remain HTTPS and within allowed hosts
            let host = attempt.url().host_str().unwrap_or("");
            let is_allowed = DEFAULT_ALLOWED_HOSTS.iter().any(|allowed| {
                host == *allowed || host.ends_with(&format!(".{allowed}"))
            });
            if !is_allowed || attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| UpdateError::DownloadFailed(e.to_string()))?;

    let mut response = client
        .get(&package.url)
        .send()
        .map_err(|e| UpdateError::DownloadFailed(e.to_string()))?;

    if !response.status().is_success() {
        return Err(UpdateError::DownloadFailed(format!("HTTP status {}", response.status())));
    }

    let total_size = response.content_length().unwrap_or(package.size_bytes);
    let mut downloaded = 0u64;
    let mut file = File::create(&staging_path).map_err(|e| UpdateError::Io(e.to_string()))?;
    let mut buffer = [0u8; 65536];

    loop {
        let count = response.read(&mut buffer).map_err(|e| {
            let _ = std::fs::remove_file(&staging_path);
            UpdateError::DownloadFailed(e.to_string())
        })?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count]).map_err(|e| {
            let _ = std::fs::remove_file(&staging_path);
            UpdateError::Io(e.to_string())
        })?;
        downloaded += count as u64;
        progress_callback(downloaded, total_size);
    }
    drop(file);

    // Verify integrity of the downloaded file
    if let Err(err) = verify_file_integrity(&staging_path, &package.sha256, package.size_bytes) {
        let _ = std::fs::remove_file(&staging_path);
        return Err(err);
    }

    // Move to verified path
    let _ = std::fs::remove_file(&verified_path);
    std::fs::rename(&staging_path, &verified_path).map_err(|e| UpdateError::Io(e.to_string()))?;

    Ok(verified_path)
}

/// Launch installation according to platform.
pub fn launch_installation(package_type: &str, file_path: &Path, is_playback_active: bool) -> Result<(), UpdateError> {
    if is_playback_active {
        return Err(UpdateError::PlaybackActive);
    }

    #[cfg(windows)]
    {
        if package_type == "installer" {
            std::process::Command::new(file_path)
                .spawn()
                .map_err(|e| UpdateError::Io(format!("failed to spawn installer: {e}")))?;
            std::process::exit(0);
        }
    }

    #[cfg(not(windows))]
    {
        if package_type == "deb" {
            // Hand off to system installer (e.g. xdg-open or software center)
            let _ = std::process::Command::new("xdg-open")
                .arg(file_path)
                .spawn();
        } else if package_type == "appimage" {
            // Check if AppImage path is writable
            if let Ok(appimage_env) = std::env::var("APPIMAGE") {
                let current_path = PathBuf::from(&appimage_env);
                if let Some(parent) = current_path.parent() {
                    let test_file = parent.join(".adult-hub-write-test.tmp");
                    if std::fs::write(&test_file, b"test").is_err() {
                        return Err(UpdateError::AppImageReadOnly(parent.display().to_string()));
                    }
                    let _ = std::fs::remove_file(&test_file);
                }

                // Prepare replacement script
                let helper_script = std::env::temp_dir().join("adult-hub-appimage-replace.sh");
                let pid = std::process::id();
                let script_content = format!(
                    "#!/bin/sh\nwhile kill -0 {pid} 2>/dev/null; do sleep 0.2; done\ncp -f \"{}\" \"{}.old\"\nmv -f \"{}\" \"{}\"\nchmod +x \"{}\"\nexec \"{}\" &\n",
                    current_path.display(),
                    current_path.display(),
                    file_path.display(),
                    current_path.display(),
                    current_path.display(),
                    current_path.display(),
                );
                std::fs::write(&helper_script, script_content)
                    .map_err(|e| UpdateError::Io(e.to_string()))?;

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&helper_script, std::fs::Permissions::from_mode(0o755));
                }

                let _ = std::process::Command::new("sh")
                    .arg(&helper_script)
                    .spawn();

                std::process::exit(0);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn test_version_comparison() {
        assert!(is_newer_version("0.2.0", "0.1.0"));
        assert!(is_newer_version("v1.0.0", "0.9.9"));
        assert!(is_newer_version("0.1.1", "0.1.0"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
        assert!(!is_newer_version("v0.1.0", "0.1.0"));
    }

    #[test]
    fn test_manifest_signature_verification_and_schema() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public_key = key_pair.public_key().as_ref();

        let manifest_json = r#"{
            "schema_version": 1,
            "version": "0.2.0",
            "release_notes_url": "https://github.com/adult-hub/adult-hub-desktop/releases/tag/v0.2.0",
            "release_notes": "Added updates and improvements",
            "packages": [
                {
                    "platform": "windows",
                    "arch": "x86_64",
                    "package_type": "installer",
                    "url": "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/adult-hub-desktop-setup.exe",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size_bytes": 62000000
                },
                {
                    "platform": "linux",
                    "arch": "x86_64",
                    "package_type": "deb",
                    "url": "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/adult-hub-desktop_0.2.0_amd64.deb",
                    "sha256": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
                    "size_bytes": 11000000
                }
            ]
        }"#;

        let sig = key_pair.sign(manifest_json.as_bytes());
        let sig_bytes = sig.as_ref();

        let parsed = parse_and_verify_manifest(
            manifest_json.as_bytes(),
            sig_bytes,
            public_key,
            DEFAULT_ALLOWED_HOSTS,
        )
        .expect("signature verification must succeed");

        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.version, "0.2.0");
        assert_eq!(parsed.packages.len(), 2);

        // Tampered manifest must fail
        let tampered_json = manifest_json.replace("0.2.0", "0.3.0");
        assert!(parse_and_verify_manifest(
            tampered_json.as_bytes(),
            sig_bytes,
            public_key,
            DEFAULT_ALLOWED_HOSTS
        )
        .is_err());

        // Corrupted signature must fail
        let mut corrupted_sig = sig_bytes.to_vec();
        corrupted_sig[0] ^= 0xff;
        assert!(parse_and_verify_manifest(
            manifest_json.as_bytes(),
            &corrupted_sig,
            public_key,
            DEFAULT_ALLOWED_HOSTS
        )
        .is_err());
    }

    #[test]
    fn test_rejects_unsupported_schema_version() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public_key = key_pair.public_key().as_ref();

        let manifest_json = r#"{
            "schema_version": 2,
            "version": "0.2.0",
            "release_notes_url": "https://github.com/adult-hub/adult-hub-desktop/releases/tag/v0.2.0",
            "packages": []
        }"#;

        let sig = key_pair.sign(manifest_json.as_bytes());
        let err = parse_and_verify_manifest(
            manifest_json.as_bytes(),
            sig.as_ref(),
            public_key,
            DEFAULT_ALLOWED_HOSTS,
        )
        .unwrap_err();

        match err {
            UpdateError::InvalidSchemaVersion(v) => assert_eq!(v, 2),
            other => panic!("expected InvalidSchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn test_rejects_insecure_or_disallowed_hosts() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public_key = key_pair.public_key().as_ref();

        // HTTP instead of HTTPS
        let insecure_json = r#"{
            "schema_version": 1,
            "version": "0.2.0",
            "release_notes_url": "http://github.com/adult-hub/releases",
            "packages": []
        }"#;
        let sig = key_pair.sign(insecure_json.as_bytes());
        assert!(parse_and_verify_manifest(
            insecure_json.as_bytes(),
            sig.as_ref(),
            public_key,
            DEFAULT_ALLOWED_HOSTS
        )
        .is_err());

        // Disallowed external domain
        let disallowed_json = r#"{
            "schema_version": 1,
            "version": "0.2.0",
            "release_notes_url": "https://github.com/adult-hub/releases",
            "packages": [
                {
                    "platform": "windows",
                    "arch": "x86_64",
                    "package_type": "installer",
                    "url": "https://malicious-server.example.com/download.exe",
                    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "size_bytes": 100
                }
            ]
        }"#;
        let sig2 = key_pair.sign(disallowed_json.as_bytes());
        let err = parse_and_verify_manifest(
            disallowed_json.as_bytes(),
            sig2.as_ref(),
            public_key,
            DEFAULT_ALLOWED_HOSTS,
        )
        .unwrap_err();

        match err {
            UpdateError::DisallowedHost(host) => assert_eq!(host, "malicious-server.example.com"),
            other => panic!("expected DisallowedHost, got {other:?}"),
        }
    }

    #[test]
    fn test_package_selection() {
        let packages = vec![
            UpdatePackage {
                platform: "windows".into(),
                arch: "x86_64".into(),
                package_type: "zip".into(),
                url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/win.zip".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 100,
            },
            UpdatePackage {
                platform: "windows".into(),
                arch: "x86_64".into(),
                package_type: "installer".into(),
                url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/win-setup.exe".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 200,
            },
            UpdatePackage {
                platform: "linux".into(),
                arch: "x86_64".into(),
                package_type: "deb".into(),
                url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/linux.deb".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 300,
            },
            UpdatePackage {
                platform: "linux".into(),
                arch: "x86_64".into(),
                package_type: "appimage".into(),
                url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/linux.AppImage".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 400,
            },
        ];

        // Windows selects installer over zip
        let win = select_best_package(&packages, "windows", "x86_64", false).unwrap();
        assert_eq!(win.package_type, "installer");

        // Linux non-appimage selects deb
        let linux_deb = select_best_package(&packages, "linux", "x86_64", false).unwrap();
        assert_eq!(linux_deb.package_type, "deb");

        // Linux appimage selects appimage
        let linux_ai = select_best_package(&packages, "linux", "x86_64", true).unwrap();
        assert_eq!(linux_ai.package_type, "appimage");
    }

    #[test]
    fn test_file_integrity_verification() {
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("adult-hub-integrity-test.bin");
        let content = b"Adult Hub Release Integrity Verification Payload 12345";
        std::fs::write(&test_file, content).unwrap();

        let mut ctx = digest::Context::new(&SHA256);
        ctx.update(content);
        let digest_hex = hex_string(ctx.finish().as_ref());

        // Correct verification
        assert!(verify_file_integrity(&test_file, &digest_hex, content.len() as u64).is_ok());

        // Size mismatch
        assert!(verify_file_integrity(&test_file, &digest_hex, (content.len() + 1) as u64).is_err());

        // Hash mismatch
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(verify_file_integrity(&test_file, wrong_hash, content.len() as u64).is_err());

        let _ = std::fs::remove_file(&test_file);
    }

    #[test]
    fn test_verify_real_generated_manifest_against_embedded_key() {
        let candidates = [
            Path::new("../../bin/adult-hub-update-manifest.json"),
            Path::new("bin/adult-hub-update-manifest.json"),
            Path::new("/work/bin/adult-hub-update-manifest.json"),
        ];
        let sig_candidates = [
            Path::new("../../bin/adult-hub-update-manifest.json.sig"),
            Path::new("bin/adult-hub-update-manifest.json.sig"),
            Path::new("/work/bin/adult-hub-update-manifest.json.sig"),
        ];

        for (m_path, s_path) in candidates.iter().zip(sig_candidates.iter()) {
            if m_path.exists() && s_path.exists() {
                let m_bytes = std::fs::read(m_path).unwrap();
                let s_bytes = std::fs::read(s_path).unwrap();
                let parsed = parse_and_verify_manifest(
                    &m_bytes,
                    &s_bytes,
                    &EMBEDDED_RELEASE_PUBLIC_KEY,
                    DEFAULT_ALLOWED_HOSTS,
                )
                .expect("Real generated manifest must authenticate with embedded key");
                assert_eq!(parsed.schema_version, 1);
                assert_eq!(parsed.version, "0.1.0");
                assert!(!parsed.packages.is_empty());
                return;
            }
        }
    }
}
