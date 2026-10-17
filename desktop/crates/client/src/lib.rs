//! Jellyfin and Resolver client library.

pub mod jellyfin;
pub mod models;
pub mod policy;
pub mod resolver;

pub use jellyfin::{GetItemsQuery, JellyfinClient, JellyfinError};
pub use models::*;
pub use policy::{ServiceUrl, ServiceUrlPolicy, ServiceUrlValidation};
pub use resolver::{ResolverClient, ResolverError};
