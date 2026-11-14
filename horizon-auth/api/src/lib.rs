//! Horizon GUI Auth: a native Rust server mod for Horizon (docs/native-mods.md,
//! "Trait mods"). It requires login before a player leaves a holding state,
//! draws the login screen as a vanilla chest menu, and stores PBKDF2 password
//! hashes in SQLite. No Java proxy, plugin or limbo server is involved.
//!
//! The launcher links this package into the game and calls [`hooks`]. The mod
//! uses only catalog hooks and `Host` methods, so it can declare the hook
//! version it needs instead of pinning a raw source target.
mod auth;
mod config;
mod menu;
mod offline;
mod password;
mod store;

use horizon_client::mod_api::Hooks;

pub fn hooks() -> Box<dyn Hooks> {
    auth::hooks()
}

#[cfg(test)]
mod tests;
