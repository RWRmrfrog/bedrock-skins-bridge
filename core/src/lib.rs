//! Core of the Bedrock Skins Bridge Pumpkin plugin: the Bedrock Skins mod's networking (custom
//! geometry skins, capes, Persona cosmetics, emotes) and Java <-> Bedrock skin translation.
//!
//! Plain Rust with no dependencies, tested natively with `cargo test`. The Pumpkin glue lives in
//! the plugin crate one directory up.

pub mod bridge;
pub mod codec;
pub mod config;
pub mod proto;
pub mod translate;

#[cfg(test)]
mod tests;
