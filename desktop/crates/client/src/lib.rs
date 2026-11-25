//! Adult Hub and Resolver client library.

pub mod adult_hub;
pub mod models;
pub mod policy;
pub mod resolver;

pub const CLIENT_NAME: &str = "Adult Hub Desktop";

pub use adult_hub::*;
pub use models::*;
pub use policy::{ServiceUrl, ServiceUrlPolicy, ServiceUrlValidation};
pub use resolver::{ResolverClient, ResolverError};

/// Bare Telegram provider reference (`<channel>:<message>:<file>`) for any
/// spelling a caller may hold: the reference itself, the catalog id
/// (`telegram:<ref>`) or a value that already carries the prefix twice
/// (`telegram:telegram:<ref>`).
///
/// Every resolver URL is built by prepending `telegram:` once, so a value that
/// arrives with a prefix already attached must be reduced first - otherwise the
/// request goes to `/admin/play/telegram:telegram:…` and the resolver rejects it.
pub fn canonical_telegram_ref(value: &str) -> &str {
    let mut bare = value.trim();
    while let Some(rest) = bare.strip_prefix("telegram:") {
        bare = rest;
    }
    bare
}

pub fn urlencoding(input: &str) -> String {
    let mut encoded = String::new();
    for b in input.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(b as char);
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", b));
            }
        }
    }
    encoded
}
