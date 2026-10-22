//! Jellyfin and Resolver client library.

pub mod discovery;
pub mod jellyfin;
pub mod models;
pub mod policy;
pub mod resolver;

pub const CLIENT_NAME: &str = "Adult Hub Desktop";

pub use discovery::discover_servers;
pub use jellyfin::{GetItemsQuery, JellyfinClient, JellyfinError};
pub use models::*;
pub use policy::{ServiceUrl, ServiceUrlPolicy, ServiceUrlValidation};
pub use resolver::{ResolverClient, ResolverError};

pub(crate) fn urlencoding(input: &str) -> String {
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
