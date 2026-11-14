//! The mod's JSON config, written once beside the server's `server.properties`
//! (`Host::config_dir`, docs/native-mods.md, API 26).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where an unauthenticated player waits. It must be far from anything the
/// player should see: F3 coordinates cannot be hidden server-side, so the
/// guarantee is that the real world is not sent before login (see README).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Point {
    pub dimension: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for Point {
    fn default() -> Self {
        Self {
            dimension: "minecraft:overworld".to_owned(),
            // Inside the default world border, far enough that the spawn region
            // is never sent to an unauthenticated client. The first player to
            // log in generates the few chunks around this point.
            x: 100_000.5,
            y: 300.0,
            z: 100_000.5,
            yaw: 0.0,
            pitch: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseMode {
    /// The world spawn (or the configured `spawn`), so every player arrives at
    /// the same place after login.
    Spawn,
    /// The position Horizon chose for this player before the holding override:
    /// the saved logout position for a returning player, the world spawn for a
    /// new one.
    Saved,
}

impl Default for ReleaseMode {
    fn default() -> Self {
        Self::Spawn
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,

    /// The holding point. All unauthenticated players share it; they are hidden
    /// from each other (permission level 1 and above still see them).
    pub holding: Point,

    /// Seconds a player may stay in the holding state. 0 disables the timeout.
    pub timeout_seconds: u64,

    /// Wrong passwords in one session before the player is kicked.
    pub max_attempts: u32,

    pub min_password_length: usize,
    pub max_password_length: usize,

    /// Concurrent unauthenticated players from one address.
    pub max_pending_per_ip: usize,
    /// Connection attempts from one address per minute (login + status pings
    /// are not counted; this is pre-login only).
    pub max_connects_per_ip_per_minute: usize,
    /// Failed password checks from one address per window, across accounts.
    pub max_login_failures_per_ip: usize,
    pub login_failure_window_seconds: u64,

    /// Failed password checks across sessions before the account is locked.
    pub account_lock_after: u32,
    pub account_lock_seconds: u64,

    /// Let operators skip the gate. Off by default: login is mandatory.
    pub allow_ops: bool,
    pub ops_level: u8,

    pub release_mode: ReleaseMode,
    /// Overrides the spawn captured from the first new player. `None` means
    /// use the captured world spawn, falling back to the player's saved point.
    pub spawn: Option<[f64; 3]>,
    pub spawn_dimension: Option<String>,
    pub spawn_yaw: f32,
    pub spawn_pitch: f32,

    /// `log2(iterations)` for PBKDF2-HMAC-SHA256. 18 is 262,144 iterations,
    /// roughly 50-150 ms per hash on a laptop. 15 is 32,768; 20 is 1,048,576.
    pub pbkdf2_log_n: u8,

    /// File name inside the mod's config folder.
    pub database: String,

    /// Vanish unauthenticated players from ordinary players and the Tab list.
    pub hide_pending_players: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            holding: Point::default(),
            timeout_seconds: 300,
            max_attempts: 3,
            min_password_length: 6,
            max_password_length: 64,
            max_pending_per_ip: 3,
            max_connects_per_ip_per_minute: 10,
            max_login_failures_per_ip: 10,
            login_failure_window_seconds: 600,
            account_lock_after: 5,
            account_lock_seconds: 300,
            allow_ops: false,
            ops_level: 4,
            release_mode: ReleaseMode::Spawn,
            spawn: None,
            spawn_dimension: None,
            spawn_yaw: 0.0,
            spawn_pitch: 0.0,
            pbkdf2_log_n: 18,
            database: "accounts.db".to_owned(),
            hide_pending_players: true,
        }
    }
}

impl Config {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("config.json")
    }

    /// Read the config, writing the defaults when it is absent. A malformed
    /// file is kept as `config.json.broken` and the defaults are used, so a
    /// typo cannot lock the server out silently.
    pub fn load_or_create(dir: &Path) -> (Self, Option<String>) {
        let path = Self::file(dir);
        if !path.exists() {
            let config = Self::default();
            if let Err(e) = config.save(dir) {
                return (config, Some(format!("could not write {}: {e}", path.display())));
            }
            return (config, None);
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(config) => (config.sanitized(), None),
                Err(e) => {
                    let broken = path.with_extension("json.broken");
                    let _ = std::fs::rename(&path, &broken);
                    let config = Self::default();
                    let _ = config.save(dir);
                    (
                        config,
                        Some(format!(
                            "{} is not valid JSON ({e}); defaults written, old file kept at {}",
                            path.display(),
                            broken.display()
                        )),
                    )
                }
            },
            Err(e) => (
                Self::default(),
                Some(format!("could not read {}: {e}", path.display())),
            ),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        let path = Self::file(dir);
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn sanitized(mut self) -> Self {
        self.max_attempts = self.max_attempts.clamp(1, 10);
        self.min_password_length = self.min_password_length.clamp(4, 64);
        self.max_password_length = self.max_password_length.clamp(self.min_password_length, 256);
        self.max_pending_per_ip = self.max_pending_per_ip.clamp(1, 64);
        self.max_connects_per_ip_per_minute = self.max_connects_per_ip_per_minute.clamp(1, 1000);
        self.max_login_failures_per_ip = self.max_login_failures_per_ip.clamp(1, 1000);
        self.account_lock_after = self.account_lock_after.clamp(1, 100);
        self.pbkdf2_log_n = self.pbkdf2_log_n.clamp(15, 20);
        self.ops_level = self.ops_level.min(4);
        if self.timeout_seconds > 86_400 {
            self.timeout_seconds = 86_400;
        }
        if self.database.is_empty() || self.database.contains(['/', '\\']) {
            self.database = "accounts.db".to_owned();
        }
        if self.holding.dimension.is_empty() {
            self.holding.dimension = "minecraft:overworld".to_owned();
        }
        self
    }
}

