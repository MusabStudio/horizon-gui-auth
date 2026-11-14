//! The gate itself: a holding state, a chest-menu login screen, command and
//! interaction suppression, and a release path. Every hook used here is a
//! catalog hook in `native-hooks.json` (docs/native-mods.md); their versions
//! are listed in README.md.
use crate::config::{Config, ReleaseMode};
use crate::menu;
use crate::offline;
use crate::password;
use crate::store::{self, Account, Store};
use horizon_client::mod_api::{
    AttackEntityEvent, CreativeSlotV2Event, DestroyBlockEvent, Flow, Host, HostTaskExt, Hooks,
    InteractEntityEvent, InventoryClickEvent, InventoryDragEvent, ItemDroppedV2Event,
    ItemPickedUpV2Event, MenuClickedEvent, MenuClosedEvent, NativeCommand, PlayerChangedDimensionEvent,
    PlayerChatEvent, PlayerCommandEvent, PlayerLoggedInEvent, PlayerLoggedOutEvent,
    PlayerLoginEvent, PlayerMoveEvent, PlayerPortalEvent, PlayerPreLoginEvent,
    PlayerRespawnPositionEvent, PlayerRespawnedEvent, PlayerSpawnLocationEvent,
    PlayerSwapHandsEvent, PlayerTeleportEvent, PlayerTickEvent, RegisterCommandsEvent,
    RightClickBlockEvent, ServerTickEvent, ServerTickPhase, Text, UseItemEvent, UseItemOnEvent,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

pub struct AuthMod {
    state: Arc<State>,
}

impl Default for AuthMod {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthMod {
    pub fn new() -> Self {
        Self {
            state: Arc::new(State::default()),
        }
    }
}

pub fn hooks() -> Box<dyn Hooks> {
    Box::new(AuthMod::new())
}

#[derive(Default)]
struct State {
    paths: OnceLock<Paths>,
    cached: RwLock<Option<Config>>,
    pending: Mutex<HashMap<u128, Pending>>,
    ip_attempts: Mutex<HashMap<String, VecDeque<Instant>>>,
    ip_failures: Mutex<HashMap<String, VecDeque<Instant>>>,
    platforms: Mutex<HashSet<String>>,
    platform_attempts: Mutex<HashMap<String, Instant>>,
    world_spawn: Mutex<Option<ReleasePoint>>,
    bypass: Mutex<HashSet<u128>>,
    changing: Mutex<HashSet<u128>>,
    last_prune: Mutex<Option<Instant>>,
}

struct Paths {
    dir: PathBuf,
    config: RwLock<Config>,
    store: Store,
}

#[derive(Clone)]
struct Pending {
    name: String,
    ip: String,
    release: ReleasePoint,
    joined: Instant,
    attempts: u32,
    menu_window: Option<i32>,
    last_menu_attempt: Instant,
    auth_in_flight: bool,
    registered: bool,
    hidden: bool,
    offline_uuid_matches: bool,
}

#[derive(Clone)]
struct ReleasePoint {
    dimension: String,
    pos: [f64; 3],
    yaw: f32,
    pitch: f32,
}

impl State {
    fn paths(&self, host: &dyn Host) -> Option<&Paths> {
        if let Some(paths) = self.paths.get() {
            return Some(paths);
        }
        let dir = host
            .config_dir()
            .ok()
            .map(|dir| dir.join("horizon-auth"))
            .or_else(|| host.world_dir().map(|dir| dir.join("horizon-auth")))?;
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("[horizon-auth] cannot create {}: {e}", dir.display());
            return None;
        }
        let (config, warning) = Config::load_or_create(&dir);
        if let Some(warning) = warning {
            eprintln!("[horizon-auth] {warning}");
        }
        let config = config.sanitized();
        let store = Store::new(dir.join(&config.database));
        if let Err(e) = store.init() {
            eprintln!("[horizon-auth] cannot open {}: {e}", store.path().display());
            return None;
        }
        let paths = Paths {
            dir,
            config: RwLock::new(config.clone()),
            store,
        };
        let _ = self.paths.set(paths);
        *self.cached.write().unwrap_or_else(|e| e.into_inner()) = Some(config);
        self.paths.get()
    }

    fn config_now(&self, host: &dyn Host) -> Config {
        self.paths(host)
            .map(|p| {
                p.config
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            })
            .or_else(|| self.config_cached())
            .unwrap_or_default()
    }

    fn config_cached(&self) -> Option<Config> {
        self.cached
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn pending_contains(&self, player: u128) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&player)
    }

    fn acting_pending(&self, host: &dyn Host) -> bool {
        host.acting_player()
            .is_some_and(|player| self.pending_contains(player.uuid))
    }

    fn release_target(&self, config: &Config, pending: &Pending) -> ReleasePoint {
        if config.release_mode == ReleaseMode::Spawn {
            if let Some(pos) = config.spawn {
                return ReleasePoint {
                    dimension: config
                        .spawn_dimension
                        .clone()
                        .unwrap_or_else(|| "minecraft:overworld".to_owned()),
                    pos,
                    yaw: config.spawn_yaw,
                    pitch: config.spawn_pitch,
                };
            }
            if let Some(spawn) = self
                .world_spawn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                return spawn;
            }
        }
        pending.release.clone()
    }

    fn release(&self, host: &mut dyn Host, player: u128) {
        let Some(pending) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&player)
        else {
            return;
        };
        let config = self.config_now(host);
        // Close first so the close hook sees no pending entry and does not reopen.
        host.close_menu(player);
        if pending.hidden {
            let _ = host.hide_player(player, None);
        }
        let target = self.release_target(&config, &pending);
        let moved = host.teleport_player(
            player,
            &target.dimension,
            target.pos,
            target.yaw,
            target.pitch,
        );
        if !moved {
            let command = format!(
                "execute in {} run tp {} {} {} {} {} {}",
                target.dimension,
                pending.name,
                target.pos[0],
                target.pos[1],
                target.pos[2],
                target.yaw,
                target.pitch
            );
            if !host.run_command(&command) {
                let _ = host.tell(
                    player,
                    Text::legacy(
                        "\u{a7}cCould not place you at spawn. An operator must check the server log.",
                    ),
                );
                let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                if !map.contains_key(&player) {
                    map.insert(
                        player,
                        Pending {
                            menu_window: None,
                            last_menu_attempt: Instant::now() - Duration::from_secs(2),
                            ..pending
                        },
                    );
                }
                return;
            }
        }
        let _ = host.tell(player, Text::legacy("\u{a7}aLogged in. Welcome!"));
        let _ = host.action_bar(player, Text::legacy("\u{a7}aYou are now in the world."));
        let _ = host.play_sound_to(
            player,
            "minecraft:entity.player.levelup",
            "master",
            None,
            0.7,
            1.4,
        );
        let _ = host.title(
            player,
            &Text::legacy("\u{a7}aWelcome"),
            &Text::legacy("\u{a7}7Authenticated"),
            10,
            40,
            20,
        );
    }

    fn open_menu(
        &self,
        host: &mut dyn Host,
        player: u128,
        config: &Config,
        registered: bool,
        attempts: u32,
    ) {
        let window = host.open_menu(
            player,
            menu::ROWS,
            menu::title(),
            menu::items(registered, attempts, config.max_attempts),
        );
        if let Some(pending) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&player)
        {
            pending.last_menu_attempt = Instant::now();
            if window.is_some() {
                pending.menu_window = window;
                pending.registered = registered;
                pending.attempts = attempts;
            }
        }
    }

    fn ensure_platform(&self, host: &mut dyn Host, config: &Config, dimension: &str) {
        let x = config.holding.x.floor() as i32;
        let y = config.holding.y.floor() as i32;
        let z = config.holding.z.floor() as i32;
        let key = format!("{dimension}:{x}:{y}:{z}");
        if self
            .platforms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&key)
        {
            return;
        }
        {
            let mut attempts = self
                .platform_attempts
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if attempts
                .get(&key)
                .is_some_and(|last| last.elapsed() < Duration::from_secs(5))
            {
                return;
            }
            attempts.insert(key.clone(), Instant::now());
        }
        let Some(barrier) = host.default_state("minecraft:barrier") else {
            return;
        };
        let mut built = false;
        for dx in -4..=4 {
            for dz in -4..=4 {
                if host.set_block(dimension, [x + dx, y - 1, z + dz], barrier, 3) {
                    built = true;
                }
            }
        }
        if built {
            self.platforms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key);
        }
    }

    fn record_ip_failure(&self, ip: &str, window: Duration) {
        if ip.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut failures = self
            .ip_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let list = failures.entry(ip.to_owned()).or_default();
        while list.front().is_some_and(|t| now.duration_since(*t) > window) {
            list.pop_front();
        }
        list.push_back(now);
    }

    fn ip_failure_count(&self, ip: &str, window: Duration) -> usize {
        if ip.is_empty() {
            return 0;
        }
        let now = Instant::now();
        self.ip_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(ip)
            .map(|list| {
                list.iter()
                    .filter(|t| now.duration_since(**t) <= window)
                    .count()
            })
            .unwrap_or(0)
    }

    fn finish_register(
        &self,
        host: &mut dyn Host,
        player: u128,
        name: String,
        ip: String,
        result: Result<(), String>,
    ) {
        let still_here = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = pending.get_mut(&player) {
                p.auth_in_flight = false;
                true
            } else {
                false
            }
        };
        match result {
            Ok(()) => {
                if still_here {
                    let _ = host.tell(player, Text::legacy("\u{a7}aRegistered and logged in."));
                }
                let _ = (name, ip);
                self.release(host, player);
            }
            Err(error) => {
                if still_here {
                    let _ = host.tell(
                        player,
                        Text::colored(format!("\u{a7}cRegistration failed: {error}"), 0xff5555),
                    );
                    let config = self.config_now(host);
                    let (registered, attempts) = self
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&player)
                        .map(|p| (p.registered, p.attempts))
                        .unwrap_or((false, 0));
                    self.open_menu(host, player, &config, registered, attempts);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_login(
        &self,
        host: &mut dyn Host,
        player: u128,
        name: String,
        ip: String,
        store: Store,
        result: Result<bool, String>,
    ) {
        let config = self.config_now(host);
        let (still_here, attempts, registered) = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            match pending.get_mut(&player) {
                Some(p) => {
                    p.auth_in_flight = false;
                    if matches!(&result, Ok(true)) {
                        (true, p.attempts, p.registered)
                    } else {
                        p.attempts = p.attempts.saturating_add(1);
                        (true, p.attempts, p.registered)
                    }
                }
                None => (false, 0, false),
            }
        };
        if !still_here {
            return;
        }
        let now = store::now_ms();
        match result {
            Ok(true) => {
                let uuid = format!("{player:032x}");
                let _ = store.success(&name, &uuid, &ip, now);
                store.audit(&name, &ip, "login", "ok", now);
                self.ip_failures
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&ip);
                let _ = host.tell(player, Text::legacy("\u{a7}aPassword accepted."));
                self.release(host, player);
            }
            Ok(false) => {
                let window = Duration::from_secs(config.login_failure_window_seconds.max(1));
                self.record_ip_failure(&ip, window);
                let (next, locked_until) = store
                    .failure(
                        &name,
                        &ip,
                        now,
                        config.account_lock_after,
                        config.account_lock_seconds,
                    )
                    .unwrap_or((attempts, 0));
                store.audit(
                    &name,
                    &ip,
                    "login",
                    &format!("wrong password attempt {next}"),
                    now,
                );
                if locked_until > now {
                    let seconds = ((locked_until - now) / 1000).max(1);
                    let _ = host.tell(
                        player,
                        Text::colored(
                            format!("\u{a7}cToo many wrong passwords. Locked for {seconds}s."),
                            0xff5555,
                        ),
                    );
                    let _ = host.kick(
                        player,
                        &Text::legacy("\u{a7}cToo many wrong passwords. Try again later."),
                    );
                    let _ = self
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&player);
                    return;
                }
                if attempts >= config.max_attempts {
                    let _ = host.kick(
                        player,
                        &Text::legacy("\u{a7}cToo many wrong passwords."),
                    );
                    let _ = self
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&player);
                    return;
                }
                let _ = host.tell(
                    player,
                    Text::colored(
                        format!(
                            "\u{a7}cWrong password ({attempts}/{}).",
                            config.max_attempts
                        ),
                        0xff5555,
                    ),
                );
                self.open_menu(host, player, &config, registered, attempts);
            }
            Err(error) => {
                let _ = host.tell(
                    player,
                    Text::colored(format!("\u{a7}cLogin failed: {error}"), 0xff5555),
                );
                self.open_menu(host, player, &config, registered, attempts);
            }
        }
    }

    fn finish_change_password(
        &self,
        host: &mut dyn Host,
        player: u128,
        result: Result<(), String>,
    ) {
        self.changing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&player);
        match result {
            Ok(()) => {
                let _ = host.tell(player, Text::legacy("\u{a7}aPassword changed."));
            }
            Err(error) => {
                let _ = host.tell(
                    player,
                    Text::colored(format!("\u{a7}cChange failed: {error}"), 0xff5555),
                );
            }
        }
    }
}

fn split_command(line: &str) -> (String, Vec<&str>) {
    let mut words = line.split_whitespace();
    let label = words.next().unwrap_or_default().to_ascii_lowercase();
    (label, words.collect())
}

fn usage_register(replies: &mut Vec<Text>) {
    replies.push(Text::legacy(
        "\u{a7}cUsage: /register <password> <password>",
    ));
}

fn usage_login(replies: &mut Vec<Text>) {
    replies.push(Text::legacy("\u{a7}cUsage: /login <password>"));
}

fn say(replies: &mut Vec<Text>, line: impl Into<String>) {
    let line = line.into();
    replies.push(Text::legacy(&line));
}

fn refuse(event: &mut PlayerPreLoginEvent<'_>, message: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(Err(Text::legacy(message)));
    event.defer.push(rx);
}

impl Hooks for AuthMod {
    fn player_pre_login(&self, event: &mut PlayerPreLoginEvent<'_>) {
        let state = &self.state;
        let config = state.config_cached().unwrap_or_default();
        if !config.enabled {
            return;
        }
        let ip = event.address.ip().to_string();
        let now = Instant::now();
        {
            let mut attempts = state
                .ip_attempts
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let list = attempts.entry(ip.clone()).or_default();
            while list
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
            {
                list.pop_front();
            }
            let too_many = list.len() >= config.max_connects_per_ip_per_minute;
            list.push_back(now);
            if too_many {
                drop(attempts);
                refuse(
                    event,
                    "\u{a7}cToo many connections from your address. Try again in a minute.",
                );
                return;
            }
        }
        let pending = state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|p| p.ip == ip)
            .count();
        if pending >= config.max_pending_per_ip {
            refuse(
                event,
                "\u{a7}cToo many unauthenticated connections from your address.",
            );
        }
    }

    fn player_login(&self, host: &mut dyn Host, event: &mut PlayerLoginEvent<'_>) -> Flow {
        let config = self.state.config_now(host);
        if !config.enabled {
            return Flow::Continue;
        }
        // `native_player_login` runs before the player is placed, so refusing
        // here is a clean disconnect screen. This gate is deliberately not a
        // refusal: everyone is accepted into the holding state and authenticates
        // there. The only bypass is the explicit operator option.
        let mut bypass = self
            .state
            .bypass
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // A connection that died between this hook and `logged_in` must not
        // leave a bypass for the next session on the same UUID.
        bypass.remove(&event.player);
        if config.allow_ops
            && host.player_permission(event.player).unwrap_or(0) >= config.ops_level
        {
            bypass.insert(event.player);
        }
        Flow::Continue
    }

    fn player_spawn_location(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerSpawnLocationEvent<'_>,
    ) {
        let state = &self.state;
        let config = state.config_now(host);
        if !config.enabled {
            return;
        }
        let before = ReleasePoint {
            dimension: event.dimension.clone(),
            pos: *event.position,
            yaw: event.rotation[0],
            pitch: event.rotation[1],
        };
        if !host.has_played_before(event.player) {
            let mut spawn = state
                .world_spawn
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if spawn.is_none() {
                *spawn = Some(before.clone());
            }
        }
        let offline_uuid_matches = offline::offline_uuid(event.name) == event.player;
        let now = Instant::now();
        let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.insert(
            event.player,
            Pending {
                name: event.name.to_owned(),
                ip: String::new(),
                release: before,
                joined: now,
                attempts: 0,
                menu_window: None,
                last_menu_attempt: now - Duration::from_secs(2),
                auth_in_flight: false,
                registered: false,
                hidden: false,
                offline_uuid_matches,
            },
        );
        drop(pending);
        event.dimension.clone_from(&config.holding.dimension);
        event.position[0] = config.holding.x;
        event.position[1] = config.holding.y;
        event.position[2] = config.holding.z;
        event.rotation[0] = config.holding.yaw;
        event.rotation[1] = config.holding.pitch;
    }

    fn player_logged_in(&self, host: &mut dyn Host, event: &mut PlayerLoggedInEvent<'_>) {
        let state = &self.state;
        let config = state.config_now(host);
        if !config.enabled {
            return;
        }
        if state.paths(host).is_none() {
            let _ = host.kick(
                event.player,
                &Text::legacy("\u{a7}cAuthentication storage is unavailable. Contact an operator."),
            );
            return;
        }
        let info = host
            .server_players()
            .into_iter()
            .find(|p| p.uuid == event.player);
        let ip = info
            .as_ref()
            .map(|p| p.address.trim().to_owned())
            .unwrap_or_default();
        let registered = state
            .paths(host)
            .and_then(|paths| paths.store.get(event.name).ok().flatten())
            .is_some();
        let offline_uuid_matches = offline::offline_uuid(event.name) == event.player;

        // If a previous join did not leave a spawn-location entry (e.g. the mod
        // was enabled while the player was already online), use where they are
        // now as the release point and put them in the holding state anyway.
        {
            let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
            let entry = pending.entry(event.player).or_insert_with(|| Pending {
                name: event.name.to_owned(),
                ip: ip.clone(),
                release: info
                    .as_ref()
                    .map(|p| ReleasePoint {
                        dimension: p.dimension.clone(),
                        pos: p.position,
                        yaw: p.yaw,
                        pitch: p.pitch,
                    })
                    .unwrap_or_else(|| ReleasePoint {
                        dimension: config.holding.dimension.clone(),
                        pos: [config.holding.x, config.holding.y, config.holding.z],
                        yaw: config.holding.yaw,
                        pitch: config.holding.pitch,
                    }),
                joined: Instant::now(),
                attempts: 0,
                menu_window: None,
                last_menu_attempt: Instant::now() - Duration::from_secs(2),
                auth_in_flight: false,
                registered,
                hidden: false,
                offline_uuid_matches,
            });
            entry.ip.clone_from(&ip);
            entry.registered = registered;
            entry.offline_uuid_matches = offline_uuid_matches;
        }

        if !ip.is_empty() {
            let same_ip = state
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|p| p.ip == ip)
                .count();
            if same_ip > config.max_pending_per_ip {
                let _ = state
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&event.player);
                let _ = host.kick(
                    event.player,
                    &Text::legacy("\u{a7}cToo many unauthenticated connections from your address."),
                );
                return;
            }
        }

        if config.hide_pending_players {
            if host.hide_player(event.player, Some(1)) {
                if let Some(p) = state
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(&event.player)
                {
                    p.hidden = true;
                }
            }
        }

        let bypass = state
            .bypass
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&event.player);
        if bypass {
            self.state.release(host, event.player);
            return;
        }

        let _ = host.tell(
            event.player,
            Text::legacy("\u{a7}8\u{a7}m--------------------------------"),
        );
        let _ = host.tell(
            event.player,
            Text::legacy("\u{a7}6Horizon Auth \u{a7}7- login required"),
        );
        if registered {
            let _ = host.tell(
                event.player,
                Text::legacy(
                    "\u{a7}7Account found. Type \u{a7}f/login <password>\u{a7}7.",
                ),
            );
        } else {
            let _ = host.tell(
                event.player,
                Text::legacy(
                    "\u{a7}7New player. Type \u{a7}f/register <password> <password>\u{a7}7.",
                ),
            );
        }
        let _ = host.tell(
            event.player,
            Text::legacy("\u{a7}7The chest menu has clickable help. Chat and movement stay locked."),
        );
        let _ = host.tell(
            event.player,
            Text::legacy("\u{a7}8\u{a7}m--------------------------------"),
        );
        let _ = host.action_bar(
            event.player,
            Text::legacy("\u{a7}eLogin required \u{a7}7\u{2022} \u{a7}f/login <password>"),
        );
        let _ = host.title(
            event.player,
            &Text::legacy("\u{a7}6Login required"),
            &Text::legacy("\u{a7}7Use /login or /register"),
            10,
            60,
            20,
        );
        state.open_menu(
            host,
            event.player,
            &config,
            registered,
            0,
        );
    }

    fn player_tick(&self, host: &mut dyn Host, _event: &mut PlayerTickEvent) {
        let state = &self.state;
        if state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            return;
        }
        let Some(player) = host.acting_player() else {
            return;
        };
        let config = state.config_now(host);
        let snapshot = state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&player.uuid)
            .cloned();
        let Some(pending) = snapshot else {
            return;
        };

        if config.timeout_seconds > 0
            && pending.joined.elapsed() >= Duration::from_secs(config.timeout_seconds)
        {
            if host.kick(
                player.uuid,
                &Text::legacy("\u{a7}cLogin timed out. Reconnect and use /login or /register."),
            ) {
                let _ = state
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&player.uuid);
            }
            return;
        }

        let holding = &config.holding;
        let dx = player.position[0] - holding.x;
        let dy = player.position[1] - holding.y;
        let dz = player.position[2] - holding.z;
        if player.dimension != holding.dimension || dx * dx + dy * dy + dz * dz > 9.0 {
            let _ = host.teleport_player(
                player.uuid,
                &holding.dimension,
                [holding.x, holding.y, holding.z],
                holding.yaw,
                holding.pitch,
            );
        }
        state.ensure_platform(host, &config, &holding.dimension);

        if pending.menu_window.is_none()
            && pending.last_menu_attempt.elapsed() > Duration::from_millis(1500)
        {
            state.open_menu(
                host,
                player.uuid,
                &config,
                pending.registered,
                pending.attempts,
            );
        }
    }

    fn player_move(&self, event: &mut PlayerMoveEvent<'_>) -> Flow {
        if self.state.pending_contains(event.player) {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }

    fn player_teleport(&self, event: &mut PlayerTeleportEvent<'_>) -> Flow {
        if !self.state.pending_contains(event.player) {
            return Flow::Continue;
        }
        let config = self.state.config_cached().unwrap_or_default();
        let holding = &config.holding;
        let near_holding = event.to_dimension == holding.dimension
            && (event.to[0] - holding.x).abs() <= 2.0
            && (event.to[1] - holding.y).abs() <= 3.0
            && (event.to[2] - holding.z).abs() <= 2.0;
        if near_holding {
            Flow::Continue
        } else {
            Flow::Cancel
        }
    }

    fn player_portal(&self, _host: &mut dyn Host, event: &mut PlayerPortalEvent<'_>) -> Flow {
        if self.state.pending_contains(event.player) {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }

    fn player_respawn_position(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerRespawnPositionEvent<'_>,
    ) {
        if !self.state.pending_contains(event.player) {
            return;
        }
        let config = self.state.config_now(host);
        event.dimension.clone_from(&config.holding.dimension);
        *event.pos = [config.holding.x, config.holding.y, config.holding.z];
        event.yaw = config.holding.yaw;
        event.pitch = config.holding.pitch;
    }

    fn player_respawned(&self, host: &mut dyn Host, event: &mut PlayerRespawnedEvent<'_>) {
        if !self.state.pending_contains(event.player) {
            return;
        }
        let config = self.state.config_now(host);
        let holding = &config.holding;
        let _ = host.teleport_player(
            event.player,
            &holding.dimension,
            [holding.x, holding.y, holding.z],
            holding.yaw,
            holding.pitch,
        );
        let (registered, attempts) = self
            .state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&event.player)
            .map(|p| (p.registered, p.attempts))
            .unwrap_or((false, 0));
        self.state
            .open_menu(host, event.player, &config, registered, attempts);
    }

    fn player_changed_dimension(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerChangedDimensionEvent<'_>,
    ) {
        if !self.state.pending_contains(event.player) {
            return;
        }
        let config = self.state.config_now(host);
        let holding = &config.holding;
        if event.to != holding.dimension.as_str() {
            let _ = host.teleport_player(
                event.player,
                &holding.dimension,
                [holding.x, holding.y, holding.z],
                holding.yaw,
                holding.pitch,
            );
        }
    }

    fn player_chat(&self, event: &mut PlayerChatEvent<'_>) -> Flow {
        if self.state.pending_contains(event.player) {
            event.replies.push(Text::legacy(
                "\u{a7}cChat is locked until you log in. Use \u{a7}f/login <password>\u{a7}c.",
            ));
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }

    fn player_command(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerCommandEvent<'_>,
    ) -> Flow {
        let state = &self.state;
        let (label, args) = split_command(event.command);
        let pending = state.pending_contains(event.player);
        if !pending {
            match label.as_str() {
                "register" | "reg" | "login" | "l" => {
                    say(
                        &mut event.replies,
                        "\u{a7}7You are already logged in.",
                    );
                    return Flow::Cancel;
                }
                "changepassword" | "changepw" | "cp" => {
                    self.change_password(host, event, &args);
                    return Flow::Cancel;
                }
                "authreload" => {
                    self.reload(host, event);
                    return Flow::Cancel;
                }
                "auth" => {
                    self.help(&mut event.replies);
                    return Flow::Cancel;
                }
                _ => return Flow::Continue,
            }
        }

        match label.as_str() {
            "register" | "reg" => self.register(host, event, &args),
            "login" | "l" => self.login(host, event, &args),
            "changepassword" | "changepw" | "cp" => {
                say(
                    &mut event.replies,
                    "\u{a7}cLog in first, then use /changepassword <old> <new>.",
                );
            }
            "auth" | "help" => self.help(&mut event.replies),
            _ => {
                say(
                    &mut event.replies,
                    "\u{a7}cYou must log in first. Use \u{a7}f/login <password>\u{a7}c or \u{a7}f/register <password> <password>\u{a7}c.",
                );
            }
        }
        Flow::Cancel
    }

    fn player_logged_out(&self, _host: &mut dyn Host, event: &mut PlayerLoggedOutEvent<'_>) {
        let state = &self.state;
        let _ = state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&event.player);
        let _ = state
            .bypass
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&event.player);
        let _ = state
            .changing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&event.player);
    }

    fn register_commands(&self, _host: &mut dyn Host, event: &mut RegisterCommandsEvent<'_>) {
        // Registered so the client shows them in its command tree (white, with
        // tab completion) instead of flagging them as unknown. Handling stays in
        // `player_command`, which cancels the line before vanilla sees it.
        for usage in [
            "register <password:word> <confirm:word>",
            "login <password:word>",
            "changepassword <old:word> <new:word>",
            "auth",
        ] {
            event.commands.push(NativeCommand::new(usage));
        }
    }

    fn menu_clicked(&self, host: &mut dyn Host, event: &mut MenuClickedEvent) {
        let state = &self.state;
        let snapshot = state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&event.uuid)
            .cloned();
        let Some(pending) = snapshot else {
            return;
        };
        if pending.menu_window != Some(event.window) {
            return;
        }
        let line = match event.slot as usize {
            menu::SLOT_INFO => Some(if pending.registered {
                "\u{a7}7Account found. Type \u{a7}f/login <password>\u{a7}7."
            } else {
                "\u{a7}7New player. Type \u{a7}f/register <password> <password>\u{a7}7."
            }),
            menu::SLOT_REGISTER => Some(if pending.registered {
                "\u{a7}7Already registered. Use \u{a7}f/login <password>\u{a7}7."
            } else {
                "\u{a7}aType \u{a7}f/register <password> <password>\u{a7}a in chat."
            }),
            menu::SLOT_LOGIN => Some(if pending.registered {
                "\u{a7}aType \u{a7}f/login <password>\u{a7}a in chat."
            } else {
                "\u{a7}7No account yet. Use \u{a7}f/register <password> <password>\u{a7}7."
            }),
            menu::SLOT_CHANGE => {
                Some("\u{a7}7After login: \u{a7}f/changepassword <old> <new>\u{a7}7.")
            }
            menu::SLOT_LEAVE => {
                let _ = host.kick(event.uuid, &Text::legacy("\u{a7}7Disconnected."));
                None
            }
            _ => None,
        };
        if let Some(line) = line {
            let _ = host.action_bar(event.uuid, Text::legacy(line));
            let _ = host.play_sound_to(
                event.uuid,
                "minecraft:ui.button.click",
                "master",
                None,
                1.0,
                1.0,
            );
        }
    }

    fn menu_closed(&self, _host: &mut dyn Host, event: &mut MenuClosedEvent) {
        let state = &self.state;
        // Do NOT reopen here. Closing the menu is how the player gets to the
        // chat box, and pressing T to open chat also closes the container; an
        // unconditional reopen made chat impossible to open at all.
        // `/auth` re-opens the menu on demand.
        let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = pending.get_mut(&event.uuid) {
            if p.menu_window == Some(event.window) {
                p.menu_window = None;
            }
        }
    }

    fn use_item_on(
        &self,
        host: &mut dyn Host,
        _event: &mut UseItemOnEvent<'_>,
    ) -> Flow {
        self.flow_for_acting_player(host)
    }

    fn use_item(&self, host: &mut dyn Host, _event: &mut UseItemEvent<'_>) -> Flow {
        self.flow_for_acting_player(host)
    }

    fn destroy_block(
        &self,
        host: &mut dyn Host,
        _event: &mut DestroyBlockEvent,
    ) -> Flow {
        self.flow_for_acting_player(host)
    }

    fn right_click_block(
        &self,
        host: &mut dyn Host,
        _event: &mut RightClickBlockEvent,
    ) -> Flow {
        self.flow_for_acting_player(host)
    }

    fn attack_entity(
        &self,
        host: &mut dyn Host,
        _event: &mut AttackEntityEvent<'_>,
    ) -> Flow {
        self.flow_for_acting_player(host)
    }

    fn interact_entity(
        &self,
        host: &mut dyn Host,
        _event: &mut InteractEntityEvent<'_>,
    ) -> Flow {
        self.flow_for_acting_player(host)
    }

    fn inventory_click(
        &self,
        _host: &mut dyn Host,
        event: &mut InventoryClickEvent<'_>,
    ) -> Flow {
        self.flow_for_player(event.player)
    }

    fn inventory_drag(
        &self,
        _host: &mut dyn Host,
        event: &mut InventoryDragEvent<'_>,
    ) -> Flow {
        self.flow_for_player(event.player)
    }

    fn item_dropped_v2(
        &self,
        _host: &mut dyn Host,
        event: &mut ItemDroppedV2Event<'_>,
    ) -> Flow {
        self.flow_for_player(event.player)
    }

    fn item_picked_up_v2(
        &self,
        _host: &mut dyn Host,
        event: &mut ItemPickedUpV2Event<'_>,
    ) -> Flow {
        self.flow_for_player(event.player)
    }

    fn creative_slot_v2(
        &self,
        _host: &mut dyn Host,
        event: &mut CreativeSlotV2Event<'_>,
    ) -> Flow {
        self.flow_for_player(event.player)
    }

    fn player_swap_hands(
        &self,
        _host: &mut dyn Host,
        event: &mut PlayerSwapHandsEvent<'_>,
    ) -> Flow {
        self.flow_for_player(event.player)
    }

    fn server_tick(&self, _host: &mut dyn Host, event: &mut ServerTickEvent<'_>) {
        if !matches!(event.phase, ServerTickPhase::ServerTickEnd { .. }) {
            return;
        }
        let state = &self.state;
        let mut last = state
            .last_prune
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        *last = Some(Instant::now());
        drop(last);
        let config = state.config_cached().unwrap_or_default();
        let now = Instant::now();
        let failure_window = Duration::from_secs(config.login_failure_window_seconds.max(1));
        let mut attempts = state
            .ip_attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        attempts.retain(|_, list| {
            while list
                .front()
                .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
            {
                list.pop_front();
            }
            !list.is_empty()
        });
        drop(attempts);
        let mut failures = state
            .ip_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        failures.retain(|_, list| {
            while list
                .front()
                .is_some_and(|t| now.duration_since(*t) > failure_window)
            {
                list.pop_front();
            }
            !list.is_empty()
        });
    }
}

impl AuthMod {
    fn flow_for_player(&self, player: u128) -> Flow {
        if self.state.pending_contains(player) {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }

    fn flow_for_acting_player(&self, host: &dyn Host) -> Flow {
        if self.state.acting_pending(host) {
            Flow::Cancel
        } else {
            Flow::Continue
        }
    }

    fn register(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerCommandEvent<'_>,
        args: &[&str],
    ) {
        let state = &self.state;
        if args.len() != 2 {
            usage_register(&mut event.replies);
            return;
        }
        if args[0] != args[1] {
            say(&mut event.replies, "\u{a7}cThe two passwords do not match.");
            return;
        }
        let config = state.config_now(host);
        if let Err(error) = password::validate(args[0], event.name, &config) {
            say(&mut event.replies, format!("\u{a7}c{error}"));
            return;
        }
        let Some(paths) = state.paths(host) else {
            say(
                &mut event.replies,
                "\u{a7}cAuthentication storage is unavailable.",
            );
            return;
        };
        match paths.store.get(event.name) {
            Ok(Some(_)) => {
                say(
                    &mut event.replies,
                    "\u{a7}cThat name is already registered. Use /login <password>.",
                );
                return;
            }
            Ok(None) => {}
            Err(error) => {
                say(
                    &mut event.replies,
                    format!("\u{a7}cStorage error: {error}"),
                );
                return;
            }
        }
        let ip = {
            let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
            let Some(p) = pending.get_mut(&event.player) else {
                say(
                    &mut event.replies,
                    "\u{a7}cNo login session; reconnect and try again.",
                );
                return;
            };
            if p.auth_in_flight {
                say(&mut event.replies, "\u{a7}7Please wait for the previous attempt.");
                return;
            }
            p.auth_in_flight = true;
            p.ip.clone()
        };
        let password = args[0].to_owned();
        let name = event.name.to_owned();
        let player = event.player;
        let uuid = format!("{player:032x}");
        let store = paths.store.clone();
        let log_n = config.pbkdf2_log_n;
        let state = Arc::clone(&self.state);
        let task_name = name.clone();
        let task_ip = ip.clone();
        host.spawn_task_with(
            move || {
                let hash = password::hash(&password, log_n)?;
                let now = store::now_ms();
                if store.create(&task_name, &uuid, &hash, &task_ip, now)? {
                    store.audit(&task_name, &task_ip, "register", "", now);
                    Ok(())
                } else {
                    Err("That name is already registered.".to_owned())
                }
            },
            move |host, result| {
                state.finish_register(host, player, name, ip, result);
            },
        );
    }

    fn login(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerCommandEvent<'_>,
        args: &[&str],
    ) {
        let state = &self.state;
        if args.len() != 1 {
            usage_login(&mut event.replies);
            return;
        }
        let config = state.config_now(host);
        let Some(paths) = state.paths(host) else {
            say(
                &mut event.replies,
                "\u{a7}cAuthentication storage is unavailable.",
            );
            return;
        };
        let account = match paths.store.get(event.name) {
            Ok(Some(account)) => account,
            Ok(None) => {
                say(
                    &mut event.replies,
                    "\u{a7}cNo account for that name. Use /register <password> <password>.",
                );
                return;
            }
            Err(error) => {
                say(
                    &mut event.replies,
                    format!("\u{a7}cStorage error: {error}"),
                );
                return;
            }
        };
        let now = store::now_ms();
        if account.locked_until > now {
            let seconds = ((account.locked_until - now) / 1000).max(1);
            say(
                &mut event.replies,
                format!("\u{a7}cAccount locked for {seconds}s after too many wrong passwords."),
            );
            return;
        }
        let ip = {
            let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
            let Some(p) = pending.get_mut(&event.player) else {
                say(
                    &mut event.replies,
                    "\u{a7}cNo login session; reconnect and try again.",
                );
                return;
            };
            if p.auth_in_flight {
                say(&mut event.replies, "\u{a7}7Please wait for the previous attempt.");
                return;
            }
            p.auth_in_flight = true;
            p.ip.clone()
        };
        let window = Duration::from_secs(config.login_failure_window_seconds.max(1));
        if state.ip_failure_count(&ip, window) >= config.max_login_failures_per_ip {
            say(
                &mut event.replies,
                "\u{a7}cToo many failed attempts from your address. Try again later.",
            );
            if let Some(p) = state
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&event.player)
            {
                p.auth_in_flight = false;
            }
            return;
        }
        let password = args[0].to_owned();
        let name = event.name.to_owned();
        let player = event.player;
        let hash = account.hash;
        let store = paths.store.clone();
        let state = Arc::clone(&self.state);
        host.spawn_task_with(
            move || password::verify(&password, &hash),
            move |host, result| {
                state.finish_login(host, player, name, ip, store, result);
            },
        );
    }

    fn change_password(
        &self,
        host: &mut dyn Host,
        event: &mut PlayerCommandEvent<'_>,
        args: &[&str],
    ) {
        let state = &self.state;
        if args.is_empty() || args.len() > 2 {
            say(
                &mut event.replies,
                "\u{a7}cUsage: /changepassword <old> <new>  or  /changepassword <new> <new>",
            );
            return;
        }
        let config = state.config_now(host);
        let new_password = args[args.len() - 1];
        if let Err(error) = password::validate(new_password, event.name, &config) {
            say(&mut event.replies, format!("\u{a7}c{error}"));
            return;
        }
        if args.len() == 2 && args[0] == new_password {
            say(
                &mut event.replies,
                "\u{a7}cThe new password must differ from the old one.",
            );
            return;
        }
        let Some(paths) = state.paths(host) else {
            say(
                &mut event.replies,
                "\u{a7}cAuthentication storage is unavailable.",
            );
            return;
        };
        {
            let mut changing = state.changing.lock().unwrap_or_else(|e| e.into_inner());
            if !changing.insert(event.player) {
                say(&mut event.replies, "\u{a7}7A password change is already running.");
                return;
            }
        }
        let old = (args.len() == 2).then(|| args[0].to_owned());
        let new = new_password.to_owned();
        let name = event.name.to_owned();
        let player = event.player;
        let log_n = config.pbkdf2_log_n;
        let store = paths.store.clone();
        let ip = state
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&player)
            .map(|p| p.ip.clone())
            .unwrap_or_default();
        let state = Arc::clone(&self.state);
        host.spawn_task_with(
            move || {
                let account: Account = store
                    .get(&name)?
                    .ok_or_else(|| "Account disappeared.".to_owned())?;
                if let Some(old) = old {
                    if !password::verify(&old, &account.hash)? {
                        return Err("Current password is wrong.".to_owned());
                    }
                }
                let hash = password::hash(&new, log_n)?;
                let now = store::now_ms();
                store.set_password(&name, &hash, now)?;
                store.audit(&name, &ip, "password", "changed", now);
                Ok(())
            },
            move |host, result| {
                state.finish_change_password(host, player, result);
            },
        );
    }

    fn reload(&self, host: &mut dyn Host, event: &mut PlayerCommandEvent<'_>) {
        let state = &self.state;
        if host.player_permission(event.player).unwrap_or(0) < 4 {
            say(
                &mut event.replies,
                "\u{a7}cYou must be an operator to reload the auth config.",
            );
            return;
        }
        let Some(paths) = state.paths(host) else {
            say(
                &mut event.replies,
                "\u{a7}cAuthentication storage is unavailable.",
            );
            return;
        };
        let (config, warning) = Config::load_or_create(&paths.dir);
        let config = config.sanitized();
        *paths.config.write().unwrap_or_else(|e| e.into_inner()) = config.clone();
        *state.cached.write().unwrap_or_else(|e| e.into_inner()) = Some(config);
        if let Some(warning) = warning {
            say(&mut event.replies, format!("\u{a7}e{warning}"));
        }
        say(&mut event.replies, "\u{a7}aHorizon Auth config reloaded.");
    }

    fn help(&self, replies: &mut Vec<Text>) {
        replies.push(Text::legacy("\u{a7}6Horizon Auth"));
        replies.push(Text::legacy(
            "\u{a7}f/register <password> <password>\u{a7}7 - create an account",
        ));
        replies.push(Text::legacy(
            "\u{a7}f/login <password>\u{a7}7 - log in",
        ));
        replies.push(Text::legacy(
            "\u{a7}f/changepassword <old> <new>\u{a7}7 - after login",
        ));
    }
}


