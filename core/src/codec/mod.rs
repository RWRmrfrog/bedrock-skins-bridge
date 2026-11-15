//! Self-contained codecs: no external crates, so the plugin stays small and every piece is
//! unit-tested on the host.

pub mod deflate;
pub mod json;
pub mod png;
pub mod sha256;
