//! In-app update check, manifest authentication, and package verification.
//!
//! Enforces:
//! - Desktop-specific signed manifest (schema version 1)
//! - Detached Ed25519 signature verification against embedded public key
//! - HTTPS-only downloads from configured release hosts
//! - SHA-256 digest and package size verification in a staging file before install
//! - Platform package handoff: Windows EXE plus runtime DLL, Linux .deb or AppImage

use ring::digest::{self, SHA256};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

/// A stalled connection must not leave the progress bar running forever, and a
/// slow link must still be able to finish a legitimate package.
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// One retry per transient failure: a reset or a dropped connection during a
/// 170 MB transfer used to end the whole update.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// Embedded release verification Ed25519 public key (32 bytes).
pub const EMBEDDED_RELEASE_PUBLIC_KEY: [u8; 32] = [
    0xc7, 0x20, 0x1f, 0xeb, 0xbc, 0x61, 0x25, 0x0b, 0x56, 0x27, 0x33, 0x78, 0x68, 0xea, 0xa3, 0xce,
    0xbd, 0x36, 0x43, 0x9d, 0xa7, 0x27, 0x57, 0x39, 0xa4, 0x4f, 0x7f, 0xfc, 0xf0, 0x7a, 0x8e, 0x43,
];

/// Default allowed release hosts for HTTPS downloads.
pub const DEFAULT_ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "githubusercontent.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-cloud.s3.amazonaws.com",
    "github-production-release-asset-2e65be.s3.amazonaws.com",
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

    #[error("update download cancelled")]
    Cancelled,

    #[error("I/O error during update: {0}")]
    Io(String),

    #[error("manifest JSON parse error: {0}")]
    Json(String),

    #[error("cannot install update while playback is active")]
    PlaybackActive,

    #[error("AppImage installation path is read-only: {0}")]
    #[allow(dead_code)]
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
    pub platform: String,     // "windows" or "linux"
    pub arch: String,         // "x86_64"
    pub package_type: String, // "exe", "runtime-dll", "deb", "appimage"
    pub url: String,          // HTTPS download URL
    pub sha256: String,       // 64-character lowercase hex string
    pub size_bytes: u64,
    /// Optional sidecar needed by a Windows executable, verified and installed together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_dll: Option<Box<UpdatePackage>>,
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
pub fn verify_signature(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    public_key_bytes: &[u8],
) -> Result<(), UpdateError> {
    let raw_sig = decode_signature_bytes(signature_bytes).ok_or(UpdateError::InvalidSignature)?;

    if raw_sig.len() != 64 || public_key_bytes.len() != 32 {
        return Err(UpdateError::InvalidSignature);
    }

    let peer_public_key = UnparsedPublicKey::new(&ED25519, public_key_bytes);
    peer_public_key
        .verify(manifest_bytes, &raw_sig)
        .map_err(|_| UpdateError::InvalidSignature)
}

/// Validate manifest URLs and hosts.
pub fn validate_package_urls(
    manifest: &UpdateManifest,
    allowed_hosts: &[&str],
) -> Result<(), UpdateError> {
    // Release notes URL must be HTTPS and stay on a trusted release host.
    if !manifest.release_notes_url.starts_with("https://") {
        return Err(UpdateError::InsecureUrl(manifest.release_notes_url.clone()));
    }
    let notes_url = url::Url::parse(&manifest.release_notes_url)
        .map_err(|e| UpdateError::InsecureUrl(format!("{}: {e}", manifest.release_notes_url)))?;
    let notes_host = notes_url.host_str().unwrap_or("");
    if !allowed_hosts
        .iter()
        .any(|allowed| notes_host == *allowed || notes_host.ends_with(&format!(".{allowed}")))
    {
        return Err(UpdateError::DisallowedHost(notes_host.to_string()));
    }

    for package in manifest
        .packages
        .iter()
        .flat_map(|package| std::iter::once(package).chain(package.runtime_dll.as_deref()))
    {
        if !package.url.starts_with("https://") {
            return Err(UpdateError::InsecureUrl(package.url.clone()));
        }

        let parsed_url = url::Url::parse(&package.url)
            .map_err(|e| UpdateError::InsecureUrl(format!("{}: {e}", package.url)))?;

        let host = parsed_url.host_str().unwrap_or("");
        let is_allowed = allowed_hosts
            .iter()
            .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")));

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
    let manifest: UpdateManifest =
        serde_json::from_slice(manifest_bytes).map_err(|e| UpdateError::Json(e.to_string()))?;

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
    fn split_semver(v: &str) -> (Vec<u64>, Option<&str>) {
        let without_build = v
            .trim()
            .trim_start_matches(['v', 'V'])
            .split('+')
            .next()
            .unwrap_or("");
        let (core, pre) = match without_build.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p)),
            _ => (without_build, None),
        };
        let nums = core
            .split('.')
            .filter_map(|part| part.parse::<u64>().ok())
            .collect();
        (nums, pre)
    }

    let (cand_parts, cand_pre) = split_semver(candidate);
    let (curr_parts, curr_pre) = split_semver(current);

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

    match (cand_pre, curr_pre) {
        (None, Some(_)) => true,
        (Some(_), None) | (None, None) => false,
        (Some(c_pre), Some(cur_pre)) => {
            let mut c_iter = c_pre.split('.');
            let mut cur_iter = cur_pre.split('.');
            loop {
                match (c_iter.next(), cur_iter.next()) {
                    (Some(a), Some(b)) => {
                        let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(na), Ok(nb)) => na.cmp(&nb),
                            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if ord != std::cmp::Ordering::Equal {
                            return ord == std::cmp::Ordering::Greater;
                        }
                    }
                    (Some(_), None) => return true,
                    (None, Some(_)) => return false,
                    (None, None) => return false,
                }
            }
        }
    }
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
        .filter(|p| {
            p.platform.eq_ignore_ascii_case(target_os) && p.arch.eq_ignore_ascii_case(target_arch)
        })
        .collect();

    if target_os == "windows" {
        matching.iter().find(|p| p.package_type == "exe").copied()
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
                .or_else(|| {
                    matching
                        .iter()
                        .find(|p| p.package_type == "appimage")
                        .copied()
                })
        }
    } else {
        matching.first().copied()
    }
}

/// Check file size and SHA-256 integrity.
pub fn verify_file_integrity(
    file_path: &Path,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<(), UpdateError> {
    let mut file = File::open(file_path).map_err(|e| UpdateError::Io(e.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|e| UpdateError::Io(e.to_string()))?;
    if metadata.len() != expected_size {
        return Err(UpdateError::SizeMismatch {
            expected: expected_size,
            actual: metadata.len(),
        });
    }

    let mut context = digest::Context::new(&SHA256);
    let mut buffer = [0u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| UpdateError::Io(e.to_string()))?;
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

impl UpdateError {
    /// A lost connection or a failed write is worth another attempt; a rejected
    /// manifest, a cancelled transfer or a digest mismatch is not.
    fn is_retryable(&self) -> bool {
        matches!(self, Self::DownloadFailed(_) | Self::Io(_))
    }
}

/// The mpv runtime that ships next to the executable.
fn installed_runtime_library() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let candidate = executable.parent()?.join("libmpv-2.dll");
    candidate.is_file().then_some(candidate)
}

/// True when the runtime already installed next to the executable matches the
/// signed package, so the release's copy does not need downloading again.
/// The mpv build is pinned per release and its bytes repeat between versions;
/// a missing or stale copy fails this check and is fetched as usual.
pub fn installed_runtime_matches(package: &UpdatePackage) -> bool {
    installed_runtime_library().is_some_and(|path| file_matches_package(&path, package))
}

/// Digest and size check shared by the download flow and the runtime probe.
fn file_matches_package(path: &Path, package: &UpdatePackage) -> bool {
    verify_file_integrity(path, &package.sha256, package.size_bytes).is_ok()
}

/// Download package into a temporary staging file and verify SHA-256 & size before returning verified path.
///
/// Transient transport failures are retried; `is_cancelled` is polled before the
/// request and between chunks so the UI can abort a transfer.
pub fn download_and_verify_package(
    package: &UpdatePackage,
    is_cancelled: &dyn Fn() -> bool,
    progress_callback: impl Fn(u64, u64),
) -> Result<PathBuf, UpdateError> {
    let progress: &dyn Fn(u64, u64) = &progress_callback;
    let mut last_error = UpdateError::DownloadFailed("download did not start".into());
    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        match download_package_once(package, is_cancelled, progress) {
            Ok(path) => return Ok(path),
            Err(error) if error.is_retryable() && attempt < DOWNLOAD_ATTEMPTS => {
                log::warn!(
                    "update download attempt {attempt}/{DOWNLOAD_ATTEMPTS} failed ({error}); retrying"
                );
                last_error = error;
                std::thread::sleep(Duration::from_millis(500 * u64::from(attempt)));
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error)
}

fn download_package_once(
    package: &UpdatePackage,
    is_cancelled: &dyn Fn() -> bool,
    progress_callback: &dyn Fn(u64, u64),
) -> Result<PathBuf, UpdateError> {
    if is_cancelled() {
        return Err(UpdateError::Cancelled);
    }
    let temp_dir = std::env::temp_dir();
    let unique = uuid::Uuid::new_v4();
    let file_stem = format!("adult-hub-update-{}-{unique}", package.package_type);
    let staging_path = temp_dir.join(format!("{file_stem}.staging"));
    let final_ext = match package.package_type.as_str() {
        "exe" => "exe",
        "deb" => "deb",
        "appimage" => "AppImage",
        "runtime-dll" => "dll",
        "zip" => "zip",
        _ => "bin",
    };
    let verified_path = temp_dir.join(format!("{file_stem}.{final_ext}"));

    // Cleanup previous staging if any
    let _ = std::fs::remove_file(&staging_path);

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many redirects");
            }
            // Ensure redirects remain HTTPS and within allowed hosts
            let host = attempt.url().host_str().unwrap_or("").to_string();
            let is_https = attempt.url().scheme() == "https";
            let is_allowed = DEFAULT_ALLOWED_HOSTS
                .iter()
                .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")));
            if !is_allowed || !is_https {
                attempt.error(format!("disallowed redirect to {host}"))
            } else {
                attempt.follow()
            }
        }))
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TOTAL_TIMEOUT)
        .build()
        .map_err(|e| UpdateError::DownloadFailed(e.to_string()))?;

    if is_cancelled() {
        return Err(UpdateError::Cancelled);
    }

    let mut response = client
        .get(&package.url)
        .send()
        .map_err(|e| UpdateError::DownloadFailed(e.to_string()))?;

    if !response.status().is_success() {
        return Err(UpdateError::DownloadFailed(format!(
            "HTTP status {}",
            response.status()
        )));
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
        if is_cancelled() {
            drop(file);
            let _ = std::fs::remove_file(&staging_path);
            return Err(UpdateError::Cancelled);
        }
        // The manifest's size is signed, so a body larger than it is never valid.
        // Stop before writing it instead of filling the disk.
        if downloaded + count as u64 > package.size_bytes {
            drop(file);
            let _ = std::fs::remove_file(&staging_path);
            return Err(UpdateError::SizeMismatch {
                expected: package.size_bytes,
                actual: downloaded + count as u64,
            });
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

/// The PowerShell script that swaps the running executable.
///
/// The application quits as soon as it has started this helper, so by the time
/// the script runs the process is normally gone: waiting for it must therefore
/// poll `Get-Process` instead of `Wait-Process`, which raises when the PID no
/// longer exists and aborted the whole replacement.
///
/// Every step retries, a failed swap rolls back to `.previous`, and each
/// outcome is appended to `%TEMP%\adult-hub-update.log`.
#[cfg_attr(not(windows), allow(dead_code))]
const WINDOWS_UPDATE_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$log = Join-Path $env:TEMP 'adult-hub-update.log'
function Log([string]$message) {
    try { Add-Content -LiteralPath $log -Value ((Get-Date -Format 's') + ' ' + $message) -ErrorAction Stop } catch { }
}
function Move-WithRetry([string]$from, [string]$to) {
    for ($i = 0; $i -lt 40; $i++) {
        try { Move-Item -LiteralPath $from -Destination $to -Force -ErrorAction Stop; return $true } catch { Start-Sleep -Milliseconds 250 }
    }
    return $false
}
function Copy-WithRetry([string]$from, [string]$to) {
    for ($i = 0; $i -lt 40; $i++) {
        try { Copy-Item -LiteralPath $from -Destination $to -Force -ErrorAction Stop; return $true } catch { Start-Sleep -Milliseconds 250 }
    }
    return $false
}
function Restore-Previous {
    if ((Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $dst)) {
        if (Move-WithRetry $backup $dst) { Log 'restored the previous executable' } else { Log 'could not restore the previous executable' }
    }
    if ($installRuntime -and (Test-Path -LiteralPath $runtimeBackup) -and -not (Test-Path -LiteralPath $runtimeDst)) {
        if (Move-WithRetry $runtimeBackup $runtimeDst) { Log 'restored the previous runtime library' } else { Log 'could not restore the previous runtime library' }
    }
}
$appPid = __APP_PID__
$installRuntime = __INSTALL_RUNTIME__
$src = $env:AH_UPDATE_SRC
$dst = $env:AH_UPDATE_DST
$stage = $dst + '.update'
$backup = $dst + '.previous'
$runtimeSrc = $env:AH_UPDATE_RUNTIME_SRC
$runtimeDst = $env:AH_UPDATE_RUNTIME_DST
$runtimeStage = $runtimeDst + '.update'
$runtimeBackup = $runtimeDst + '.previous'
Log ('start; pid=' + $appPid + '; src=' + $src + '; dst=' + $dst)
for ($i = 0; $i -lt 300; $i++) {
    if (-not (Get-Process -Id $appPid -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 200
}
$failure = ''
try {
    if (-not (Copy-WithRetry $src $stage)) { throw 'could not stage the downloaded executable' }
    Log 'staged executable'
    if ((Test-Path -LiteralPath $dst) -and -not (Move-WithRetry $dst $backup)) { throw 'could not move the running executable aside' }
    Log 'backed up executable'
    if ($installRuntime -and (Test-Path -LiteralPath $runtimeSrc)) {
        if (-not (Copy-WithRetry $runtimeSrc $runtimeStage)) { throw 'could not stage the runtime library' }
        if ((Test-Path -LiteralPath $runtimeDst) -and -not (Move-WithRetry $runtimeDst $runtimeBackup)) { throw 'could not move the runtime library aside' }
        Log 'staged runtime library'
    }
    if (-not (Move-WithRetry $stage $dst)) { throw 'could not install the new executable' }
    Log 'installed executable'
    if ($installRuntime -and (Test-Path -LiteralPath $runtimeStage)) {
        if (-not (Move-WithRetry $runtimeStage $runtimeDst)) { throw 'could not install the runtime library' }
        Log 'installed runtime library'
    }
} catch {
    $failure = $_.Exception.Message
}
if ($failure -ne '') {
    Log ('error: ' + $failure)
    Restore-Previous
    Remove-Item -LiteralPath $stage -Force -ErrorAction SilentlyContinue
    if ($installRuntime) { Remove-Item -LiteralPath $runtimeStage -Force -ErrorAction SilentlyContinue }
    exit 1
}
$started = $null
try { $started = Start-Process -FilePath $dst -WorkingDirectory (Split-Path -Parent $dst) -PassThru -ErrorAction Stop } catch { Log ('restart failed: ' + $_.Exception.Message) }
if (-not $started) {
    Log 'the new executable did not start; restoring the previous executable'
    if (Move-WithRetry $dst $stage) { Restore-Previous }
    exit 1
}
Start-Sleep -Seconds 4
if ($started.HasExited) {
    Log ('the new executable exited with code ' + $started.ExitCode + '; restoring the previous executable')
    if (Move-WithRetry $dst $stage) { Restore-Previous }
    exit 1
}
Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
if ($installRuntime) { Remove-Item -LiteralPath $runtimeBackup -Force -ErrorAction SilentlyContinue }
Remove-Item -LiteralPath $src -Force -ErrorAction SilentlyContinue
if ($installRuntime) { Remove-Item -LiteralPath $runtimeSrc -Force -ErrorAction SilentlyContinue }
Log 'done'
exit 0
"#;

/// Fills in the process id and the runtime flag; paths stay in the environment
/// so no quote character can end a PowerShell string literal.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_update_script(app_pid: u32, install_runtime: bool) -> String {
    WINDOWS_UPDATE_SCRIPT
        .replace("__APP_PID__", &app_pid.to_string())
        .replace(
            "__INSTALL_RUNTIME__",
            if install_runtime { "$true" } else { "$false" },
        )
}

/// Launch installation according to platform.
pub fn launch_installation(
    package_type: &str,
    file_path: &Path,
    expected_integrity: Option<(&str, u64)>,
    runtime_dll_path: Option<&Path>,
    expected_runtime_integrity: Option<(&str, u64)>,
    is_playback_active: bool,
) -> Result<(), UpdateError> {
    #[cfg(not(windows))]
    let _ = (runtime_dll_path, expected_runtime_integrity);

    if is_playback_active {
        return Err(UpdateError::PlaybackActive);
    }

    if let Some((sha256, size_bytes)) = expected_integrity {
        verify_file_integrity(file_path, sha256, size_bytes)?;
    }
    #[cfg(windows)]
    if let (Some(dll_path), Some((sha256, size_bytes))) =
        (runtime_dll_path, expected_runtime_integrity)
    {
        verify_file_integrity(dll_path, sha256, size_bytes)?;
    }

    #[cfg(windows)]
    {
        if package_type == "exe" {
            let current_exe = std::env::current_exe()
                .map_err(|e| UpdateError::Io(format!("cannot locate current executable: {e}")))?;
            let parent = current_exe.parent().ok_or_else(|| {
                UpdateError::Io("current executable has no parent directory".into())
            })?;
            let write_probe = parent.join(format!(
                ".adult-hub-update-{}-{}.tmp",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::write(&write_probe, b"update-write-check").map_err(|e| {
                UpdateError::Io(format!(
                    "application directory is not writable for the update: {e}"
                ))
            })?;
            let _ = std::fs::remove_file(write_probe);

            // Paths reach PowerShell only through environment variables. They are
            // never spliced into the script text, so no quote character in a path
            // can end a string literal.
            let runtime_destination = parent.join("libmpv-2.dll");
            let replace_script =
                windows_update_script(std::process::id(), runtime_dll_path.is_some());
            let powershell_bin = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .map(|root| {
                    root.join("System32")
                        .join("WindowsPowerShell")
                        .join("v1.0")
                        .join("powershell.exe")
                })
                .filter(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from("powershell.exe"));
            let mut command = std::process::Command::new(powershell_bin);
            command
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-NoLogo",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &replace_script,
                ])
                .env("AH_UPDATE_SRC", file_path)
                .env("AH_UPDATE_DST", &current_exe)
                .env(
                    "AH_UPDATE_RUNTIME_SRC",
                    runtime_dll_path.unwrap_or(file_path),
                )
                .env("AH_UPDATE_RUNTIME_DST", &runtime_destination);
            // CREATE_NO_WINDOW: a helper that flashes a console window looks
            // like a crash to the user, and there is nothing to read there.
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
            command
                .spawn()
                .map_err(|e| UpdateError::Io(format!("failed to schedule EXE replacement: {e}")))?;
            std::process::exit(0);
        } else {
            Err(UpdateError::DownloadFailed(format!(
                "Windows updates require an EXE package, got package type '{package_type}'"
            )))
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

                // Prepare replacement script inside a private, randomised
                // directory. A fixed path in /tmp would be vulnerable to a
                // local symlink or file-replacement race.
                let helper_dir =
                    std::env::temp_dir().join(format!("adult-hub-update-{}", uuid::Uuid::new_v4()));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    std::fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(&helper_dir)
                        .map_err(|e| UpdateError::Io(e.to_string()))?;
                }
                #[cfg(not(unix))]
                {
                    std::fs::create_dir_all(&helper_dir)
                        .map_err(|e| UpdateError::Io(e.to_string()))?;
                }

                let helper_script = helper_dir.join("replace-appimage.sh");
                let pid = std::process::id();
                let escape_arg = |p: &Path| -> String {
                    format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
                };
                let cur_esc = escape_arg(&current_path);
                let cur_old_esc = escape_arg(&current_path.with_extension("AppImage.old"));
                let file_esc = escape_arg(file_path);
                let script_content = format!(
                    "#!/bin/sh\nwhile kill -0 {pid} 2>/dev/null; do sleep 0.2; done\ncp -f {cur_esc} {cur_old_esc}\nmv -f {file_esc} {cur_esc}\nchmod +x {cur_esc}\nrm -f \"$0\"\nrmdir \"$(dirname \"$0\")\" 2>/dev/null || true\nexec {cur_esc} &\n"
                );
                let mut helper_options = std::fs::OpenOptions::new();
                helper_options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    helper_options.mode(0o700);
                }
                let mut helper_file = helper_options
                    .open(&helper_script)
                    .map_err(|e| UpdateError::Io(e.to_string()))?;
                helper_file
                    .write_all(script_content.as_bytes())
                    .map_err(|e| UpdateError::Io(e.to_string()))?;
                drop(helper_file);

                let _ = std::process::Command::new("/bin/sh")
                    .arg(&helper_script)
                    .spawn();

                std::process::exit(0);
            }
        }

        Ok(())
    }
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
        assert!(is_newer_version("1.0.0", "1.0.0-beta"));
        assert!(!is_newer_version("1.0.0-beta", "1.0.0"));
        assert!(is_newer_version("1.0.0-rc.2", "1.0.0-beta.1"));
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
                    "package_type": "exe",
                    "url": "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/adult-hub-desktop.exe",
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
        assert!(
            parse_and_verify_manifest(
                tampered_json.as_bytes(),
                sig_bytes,
                public_key,
                DEFAULT_ALLOWED_HOSTS
            )
            .is_err()
        );

        // Corrupted signature must fail
        let mut corrupted_sig = sig_bytes.to_vec();
        corrupted_sig[0] ^= 0xff;
        assert!(
            parse_and_verify_manifest(
                manifest_json.as_bytes(),
                &corrupted_sig,
                public_key,
                DEFAULT_ALLOWED_HOSTS
            )
            .is_err()
        );
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
        assert!(
            parse_and_verify_manifest(
                insecure_json.as_bytes(),
                sig.as_ref(),
                public_key,
                DEFAULT_ALLOWED_HOSTS
            )
            .is_err()
        );

        // Disallowed external domain
        let disallowed_json = r#"{
            "schema_version": 1,
            "version": "0.2.0",
            "release_notes_url": "https://github.com/adult-hub/releases",
            "packages": [
                {
                    "platform": "windows",
                    "arch": "x86_64",
                    "package_type": "exe",
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
                runtime_dll: None,
            },
            UpdatePackage {
                platform: "windows".into(),
                arch: "x86_64".into(),
                package_type: "exe".into(),
                url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/adult-hub-desktop.exe".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 200,
                runtime_dll: None,
            },
            UpdatePackage {
                platform: "linux".into(),
                arch: "x86_64".into(),
                package_type: "deb".into(),
                url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/linux.deb".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 300,
                runtime_dll: None,
            },
            UpdatePackage {
                platform: "linux".into(),
                arch: "x86_64".into(),
                package_type: "appimage".into(),
                url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/v0.2.0/linux.AppImage".into(),
                sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                size_bytes: 400,
                runtime_dll: None,
            },
        ];

        // Windows selects the executable; its optional runtime sidecar stays attached.
        let win = select_best_package(&packages, "windows", "x86_64", false).unwrap();
        assert_eq!(win.package_type, "exe");

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
        assert!(
            verify_file_integrity(&test_file, &digest_hex, (content.len() + 1) as u64).is_err()
        );

        // Hash mismatch
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(verify_file_integrity(&test_file, wrong_hash, content.len() as u64).is_err());

        let _ = std::fs::remove_file(&test_file);
    }

    /// The helper starts after the app has already quit, so waiting on the PID
    /// must not raise: `Wait-Process` did, which aborted the replacement and
    /// left the old executable in place.
    #[test]
    fn test_windows_update_script_waits_without_a_live_process() {
        let script = windows_update_script(4242, true);
        assert!(script.contains("$appPid = 4242"));
        assert!(
            !script.contains("Wait-Process"),
            "Wait-Process raises once the PID is gone, which is the normal case"
        );
        assert!(script.contains("Get-Process -Id $appPid -ErrorAction SilentlyContinue"));
        assert!(script.contains("function Move-WithRetry"));
        assert!(script.contains("function Copy-WithRetry"));
        assert!(script.contains("Restore-Previous"));
        assert!(script.contains("adult-hub-update.log"));
    }

    #[test]
    fn test_windows_update_script_stays_parseable_and_flagged() {
        for install_runtime in [true, false] {
            let script = windows_update_script(7, install_runtime);
            assert_eq!(
                script.matches('{').count(),
                script.matches('}').count(),
                "unbalanced braces would make PowerShell reject the whole script"
            );
            let flag = if install_runtime { "$true" } else { "$false" };
            assert!(script.contains(&format!("$installRuntime = {flag}")));
            assert!(script.contains("$src = $env:AH_UPDATE_SRC"));
            assert!(script.contains("$dst = $env:AH_UPDATE_DST"));
            // Paths travel through the environment only; inlining them would let
            // a quote in a user path break the script.
            assert!(!script.contains("__APP_PID__"));
        }
    }

    #[test]
    fn test_windows_update_script_restores_the_previous_build_on_failure() {
        let script = windows_update_script(1, true);
        // Every failure path must roll back rather than leave the installation
        // without an executable.
        assert!(script.contains("Move-WithRetry $backup $dst"));
        assert!(script.contains("Move-WithRetry $runtimeBackup $runtimeDst"));
        assert!(script.contains("$started.HasExited"));
        // The backup is only dropped after the new build is known to run.
        let cleanup = script
            .find("Remove-Item -LiteralPath $backup -Force")
            .unwrap();
        let liveness = script.find("$started.HasExited").unwrap();
        assert!(cleanup > liveness);
    }

    #[test]
    fn test_runtime_probe_accepts_only_the_installed_build() {
        let dir = std::env::temp_dir().join(format!("adult-hub-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let runtime = dir.join("libmpv-2.dll");
        let content = b"pinned libmpv build";
        std::fs::write(&runtime, content).unwrap();

        let mut ctx = digest::Context::new(&SHA256);
        ctx.update(content);
        let package = UpdatePackage {
            platform: "windows".into(),
            arch: "x86_64".into(),
            package_type: "runtime-dll".into(),
            url: "https://github.com/adult-hub/adult-hub-desktop/releases/download/desktop-v0.0.0/libmpv-2.dll".into(),
            sha256: hex_string(ctx.finish().as_ref()),
            size_bytes: content.len() as u64,
            runtime_dll: None,
        };

        // Installed by the same release the manifest describes: skip the download.
        assert!(file_matches_package(&runtime, &package));

        // Bumping the pinned mpv build changes the bytes, so it is fetched again.
        std::fs::write(&runtime, b"next libmpv build").unwrap();
        assert!(!file_matches_package(&runtime, &package));

        // A runtime the user removed or moved is never treated as current.
        std::fs::remove_file(&runtime).unwrap();
        assert!(!file_matches_package(&runtime, &package));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Directories that can hold a generated manifest, most specific first: the
    /// crate itself, the packaging output next to `desktop/`, the repository
    /// `bin/` used by CI, and the docker flow's `/work/bin`.
    fn manifest_directories() -> Vec<PathBuf> {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        vec![
            crate_dir.join("bin"),
            crate_dir.join("../../bin"),
            crate_dir.join("../../../bin"),
            PathBuf::from("/work/bin"),
        ]
    }

    #[test]
    fn test_verify_real_generated_manifest_against_embedded_key() {
        let searched = manifest_directories();
        for directory in &searched {
            let manifest_path = directory.join("adult-hub-update-manifest.json");
            let signature_path = directory.join("adult-hub-update-manifest.json.sig");
            if !manifest_path.exists() || !signature_path.exists() {
                continue;
            }
            let manifest_bytes = std::fs::read(&manifest_path).unwrap();
            let signature_bytes = std::fs::read(&signature_path).unwrap();
            let parsed = parse_and_verify_manifest(
                &manifest_bytes,
                &signature_bytes,
                &EMBEDDED_RELEASE_PUBLIC_KEY,
                DEFAULT_ALLOWED_HOSTS,
            )
            .expect("Real generated manifest must authenticate with embedded key");
            assert_eq!(parsed.schema_version, 1);
            assert!(!parsed.version.is_empty());
            assert!(!parsed.packages.is_empty());
            return;
        }
        // `bin/` only holds a manifest after scripts/sign-release.sh has run, so
        // this used to return without asserting anything and looked like
        // coverage. The release gate sets the variable with the published files.
        assert!(
            std::env::var_os("ADULT_HUB_REQUIRE_REAL_MANIFEST").is_none(),
            "ADULT_HUB_REQUIRE_REAL_MANIFEST is set but no manifest was found in {searched:?}"
        );
        println!("skipping: no generated manifest in {searched:?}");
    }

    #[test]
    fn test_allowed_release_hosts_matching() {
        let is_host_allowed = |host: &str| -> bool {
            DEFAULT_ALLOWED_HOSTS
                .iter()
                .any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
        };

        // Allowed hosts & subdomains
        assert!(is_host_allowed("github.com"));
        assert!(is_host_allowed("raw.githubusercontent.com"));
        assert!(is_host_allowed("objects.githubusercontent.com"));
        assert!(is_host_allowed("release-assets.githubusercontent.com"));
        assert!(is_host_allowed("any-subdomain.githubusercontent.com"));
        assert!(is_host_allowed("github-cloud.s3.amazonaws.com"));
        assert!(is_host_allowed(
            "github-production-release-asset-2e65be.s3.amazonaws.com"
        ));

        // Disallowed hosts
        assert!(!is_host_allowed("fakegithub.com"));
        assert!(!is_host_allowed("github.com.attacker.com"));
        assert!(!is_host_allowed("malicious-server.example.com"));
        assert!(!is_host_allowed("s3.amazonaws.com"));
    }
}
