//! Jellyfin and Resolver client library.

pub mod discovery;
pub mod jellyfin;
pub mod models;
pub mod policy;
pub mod resolver;

pub use discovery::discover_servers;
pub use jellyfin::{GetItemsQuery, JellyfinClient, JellyfinError};
pub use models::*;
pub use policy::{ServiceUrl, ServiceUrlPolicy, ServiceUrlValidation};
pub use resolver::{ResolverClient, ResolverError};
