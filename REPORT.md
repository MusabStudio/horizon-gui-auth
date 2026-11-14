# Horizon GUI Auth — what was created and where it is

Date: 2026-11-12  
Workspace: `C:\D\Projects\mods for my rust\gui auth plugin`  
Horizon source used for the API: `C:\D\Projects\mods for my rust\via version for rsut\horizon-src`  
Horizon commit pinned: `084b20d5cfbb104d4a70f61091198831074619f1` (API 415)

---

## 1. Short answer

I created a **native Horizon Rust server mod source bundle** at:

```text
C:\D\Projects\mods for my rust\gui auth plugin\horizon-auth\
```

It is a **trait mod** (docs/native-mods.md, “Trait mods”), not a Java plugin, not a
proxy, and not a limbo server. It compiles against Horizon’s real `mod_api`.

**It is not yet a finished `.hnmod` or a built server engine.** The mod crate
compiles; the launcher-based engine export and a real in-game join test have not
been run. Details are in §6.

---

## 2. Files created

```text
C:\D\Projects\mods for my rust\gui auth plugin\
├── REPORT.md                          ← this report
└── horizon-auth\                      ← the native mod bundle
    ├── horizon-mixin.toml             ← bundle manifest, pins Horizon commit
    └── api\
        ├── Cargo.toml                 ← mod package + allowed deps only
        └── src\
            ├── lib.rs                 ← exports hooks()
            ├── auth.rs                ← gate, hooks, commands, async auth (1620 lines)
            ├── config.rs              ← JSON config, defaults, validation
            ├── menu.rs                ← chest-menu items: Register / Login / Change / Leave
            ├── offline.rs             ← MD5 offline UUID (OfflinePlayer:<name>)
            ├── password.rs            ← scrypt hashing + constant-time verify
            └── store.rs               ← SQLite accounts, audit, lockout
```

`api\target\` is only local Cargo build cache from the compile check. It is not
part of the mod.

A temporary `horizon-auth\.cargo\config.toml` was used to point the check at the
local Horizon clone. **It has been removed** so the bundle is clean for the
launcher. To reproduce the local check, see §7.

---

## 3. What the mod does

### Login flow

```text
connect
  → player accepted by the server
  → player_spawn_location: capture real spawn / saved position,
                            override placement to the holding point
  → player_logged_in: vanish player, open a 3-row chest GUI,
                      send chat + title instructions
  → holding state: movement, chat, commands, interactions, item moves blocked
  → /register <pw> <pw>  or  /login <pw>
  → scrypt hash/verify on a worker thread
  → success: remove gate, close GUI, show player, teleport to spawn
  → normal play (no per-tick work for authenticated players)
```

### GUI (vanilla chest menu, 3 rows = 27 slots)

Built with ordinary item stacks, so a stock 26.3 client renders it:

| Slot | Item | Meaning |
|---:|---|---|
| 4 | Knowledge book | status / attempts |
| 11 | Lime dye / gray dye | **Register** |
| 13 | Paper / gray dye | **Login** |
| 15 | Book | **Change password** |
| 22 | Barrier | **Disconnect** |
| border | Black stained-glass pane | frame |

Chest menus have **no text input field**, so the buttons show the command to
type. The passwords are typed as commands, never as GUI text.

### Commands handled

| Command | When | Action |
|---|---|---|
| `/register <pw> <pw>` | unauthenticated | create account, hash, auto-login |
| `/login <pw>` | unauthenticated | verify, release |
| `/changepassword <old> <new>` | authenticated | verify old, set new |
| `/changepassword <new> <new>` | authenticated | set new (session already proven) |
| `/auth` | any | help |
| `/authreload` | operator level 4 | reload config |

Aliases: `reg`, `l`, `changepw`, `cp`.

### Suppression while unauthenticated

- `player_move` → cancel (server sends them back to the holding point)
- `player_chat` → cancel, answer through `replies`
- `player_command` → cancel everything except the auth commands
- `use_item_on`, `use_item`, `destroy_block`, `right_click_block`,
  `attack_entity`, `interact_entity` → cancel
- `inventory_click`, `inventory_drag`, `item_dropped_v2`,
  `item_picked_up_v2`, `creative_slot_v2`, `player_swap_hands` → cancel
- `player_portal`, `player_teleport` → cancel except into the holding point
- death/respawn → forced back to the holding point and GUI reopened

### Holding state

- Default: `minecraft:overworld` at `100000.5, 300, 100000.5` (configurable).
- A 9×9 barrier platform is built once around the feet.
- Position is re-enforced if the player leaves a 3-block radius.
- The player is vanished from players below permission level 1
  (`Host::hide_player`, API 314).
- On release, the player is shown again and teleported to the configured spawn
  or to the position Horizon originally chose for them.

**F3 caveat:** server-side code cannot hide F3 coordinates. The protection is
that the real spawn/base region is never sent before login; a player in the
holding state is 100,000 blocks away. This is stated plainly in the mod config
and in the report.

### Storage

- SQLite database: `<engine config folder>/horizon-auth/accounts.db`
- `config.json` in the same folder, written with defaults on first run.
- Tables:
  - `accounts(name COLLATE NOCASE PRIMARY KEY, uuid, hash, created_at,
    updated_at, last_ip, last_login, failed, locked_until)`
  - `audit(id, at, name, ip, action, detail)`
- **Never stores plaintext passwords.**
- Hash format: `scrypt$<log_n>$<r>$<p>$<salt_b64>$<hash_b64>`
- 16-byte random salt per password, 32-byte output, constant-time compare.

### Limits / security defaults

| Setting | Default |
|---|---:|
| `timeout_seconds` | 300 |
| `max_attempts` per session | 3 (then kick) |
| `account_lock_after` across sessions | 5 (then 300 s lock) |
| `max_pending_per_ip` | 3 |
| `max_connects_per_ip_per_minute` | 10 |
| `max_login_failures_per_ip` | 10 per 10 min |
| `min_password_length` | 6 |
| `max_password_length` | 64 |
| `scrypt_log_n` | 15 (32 MiB, ~50–100 ms/hash) |
| `allow_ops` | `false` (login is mandatory) |

Hashing runs on Horizon’s mod worker pool, not the server thread
(`Host::spawn_task` / `spawn_task_with`; API `mod_async_worker`).

---

## 4. Horizon hooks used (and why)

All are catalog hooks from `native-hooks.json` / docs/native-mods.md, so the
bundle can declare API 415 instead of pinning a raw source target.

| Hook | API | Use |
|---|---:|---|
| `player_pre_login` | pre-login | per-IP connection rate limit; can refuse asynchronously |
| `player_login` | 172 | operator bypass decision; cannot send chat/menu yet |
| `player_spawn_location` | @API | capture real spawn/saved point, override to holding **before** world is sent |
| `player_logged_in` | 57 | open GUI, vanish, welcome text |
| `player_tick` | 31 | timeout, position enforcement, menu reopen, platform build |
| `player_move` | 21 | freeze |
| `player_teleport` | 21 | allow only holding-point teleports |
| `player_portal` | — | block portals |
| `player_respawn_position` / `player_respawned` | — | keep dead players in holding |
| `player_chat` | 399 | block chat |
| `player_command` | 95 | intercept `/register`, `/login`, `/changepassword` |
| `menu_clicked` / `menu_closed` | 10 | GUI buttons / reopen on ESC |
| `inventory_click` / `inventory_drag` | — | block item moves |
| `item_dropped_v2` / `item_picked_up_v2` / `creative_slot_v2` | — | block item changes |
| `use_item_on` / `use_item` / `destroy_block` / `right_click_block` | 13, 14, 21 | block world use |
| `attack_entity` / `interact_entity` | 22, 29 | block entity use |
| `player_swap_hands` | — | block inventory swap |
| `player_logged_out` | 57 | clean session |
| `server_tick` | 4 | prune IP rate maps every 30 s |

`Host` helpers used: `config_dir` (26), `world_dir`, `default_state` +
`set_block` (24), `open_menu` / `close_menu` / `set_menu_items` (10),
`teleport_player` (21), `hide_player` (314), `kick` (`@API:kick`),
`tell`, `action_bar`, `title`, `play_sound_to`, `run_command`,
`player_permission`, `server_players`, `acting_player`, `has_played_before`,
`spawn_task`.

---

## 5. Research findings (what exists, what does not)

### Exists and is enough for this mod
- A joining-player hook that can refuse (`player_login`, API 172), but it runs
  before the player is placed, so it cannot open a GUI or send chat.
- `player_logged_in` (API 57) after placement, where menus and messages work.
- `player_spawn_location`, which changes the placement **before the world around
  the player is sent**. This is the key hook that keeps the real spawn out of an
  unauthenticated client.
- Server-side chest menus (`agent 10`) with click routing and no item movement.
- Movement cancellation (`player_move`), chat cancellation (`player_chat`),
  command interception (`player_command`), interaction cancellation.
- `Host::address` (API 407) for per-IP limits.
- `Host::kick`, `Host::teleport_player`, `Host::hide_player`, `Host::config_dir`,
  `Host::set_block`, `Host::default_state`.

### Missing / limitations worked around
- **No text-input GUI.** Vanilla chest menus only have slots. Passwords must be
  typed as commands; the GUI is a clear visual guide and button surface.
- **No server-side F3 hiding.** Cannot be done; the holding point is far away and
  the real world is not sent first.
- **No direct “get world spawn” helper.** Captured from the first new player’s
  `player_spawn_location`; an explicit `spawn` config overrides it.
- **`player_login` cannot tell the player why.** It can only cancel with a
  disconnect reason; this mod accepts everyone into holding instead.
- **`player_move` and `player_chat` have no `Host`.** Chat feedback goes through
  `event.replies`; movement has nothing to say.
- **No `player_tick` player field.** Use `host.acting_player()` inside the tick
  hook, as the API documents.

### Offline UUIDs
- Java offline UUID = `UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8))`.
- That is MD5, with version 3 and IETF variant bits written into bytes 6 and 8.
- Verified examples:
  - `Notch` → `b50ad385-829d-3141-a216-7e7d7539ba7f`
  - `Steve` → `5627dd98-e6be-3c21-b8a8-e92344183641`
  - `Alex` → `36532b5e-c442-3dbb-a24c-c7e55d0f979a`
- The mod stores the joining UUID and the computed offline UUID. Accounts are
  keyed by **lowercased name** (`COLLATE NOCASE`), because in offline mode the
  name is the stable identity and the UUID is a pure function of it. This also
  keeps an account with the person if the server later switches to online mode.

### Password hashing choice
The brief suggested argon2 or bcrypt. A native mod may use only crates already
in Horizon’s `Cargo.lock`, and the lock has **scrypt 0.11**, not argon2/bcrypt
(`docs/native-mods.md`, “Dependencies come from Horizon’s `Cargo.lock`”;
`crates/horizon-mod-deps/Cargo.toml`). The mod therefore uses scrypt, which is
memory-hard and stronger than a bare SHA/PBKDF2 setup. This is a deliberate,
documented substitution, not an invented API.

---

## 6. Build / verification status (honest)

| Item | Status |
|---|---|
| Mod source written | **Done** |
| `cargo check` against Horizon API 415 | **Passed** |
| Unit tests (password/offline) | Started; **stopped on request**, no pass/fail result claimed |
| `.hnmod` packaged | **Not done** |
| Standalone `horizon-engine` export | **Not done** |
| Real client join / new player / wrong password ×3 / timeout / reconnect | **Not tested** |
| Ubuntu deployment unit | Written below; **not executed** |

### Why the engine was not built
The normal path is the Horizon launcher:

```sh
horizon-launcher native-build <mod-folder> \
  --server-output <empty-export-dir> \
  --version <release-or-nightly> \
  --trust-native-code
```

`horizon-launcher` is **not installed on this PC**. The installed Horizon release
has `horizon-server`, `horizon-client` and `horizon-prepare`, but no launcher and
no full engine-export tool. Building the launcher from source is a separate
large build (and the workspace excludes it). The source clone compiles, but it
does not contain the prepared `native-data/` resources needed to run a server.

Also, this PC is Windows. A Windows `.exe` cannot be deployed to Ubuntu 24.04.
Horizon’s own docs say the export runs on the **same OS/architecture** and does
not cross-compile. Build the engine on Ubuntu 24.04 (or WSL2 Ubuntu) and copy
only the finished export to the server. Do **not** build on the 952 MB VM.

---

## 7. How to reproduce the local type-check

The final `api/Cargo.toml` depends on the GitHub commit:

```toml
horizon-client = { git = "https://github.com/horizon-rust/horizon", rev = "084b20d5..." }
```

To type-check against the existing local clone without changing the bundle, put
this in `horizon-auth/.cargo/config.toml` temporarily, run `cargo check`, then
delete it:

```toml
[patch."https://github.com/horizon-rust/horizon"]
horizon-client = { path = "C:/D/Projects/mods for my rust/via version for rsut/horizon-src/crates/horizon-client" }

[patch.crates-io]
wgpu-hal = { path = "C:/D/Projects/mods for my rust/via version for rsut/horizon-src/third_party/wgpu-hal" }
naga = { path = "C:/D/Projects/mods for my rust/via version for rsut/horizon-src/third_party/naga" }
libmimalloc-sys = { path = "C:/D/Projects/mods for my rust/via version for rsut/horizon-src/third_party/libmimalloc-sys" }
```

Then:

```powershell
cd "C:\D\Projects\mods for my rust\gui auth plugin\horizon-auth\api"
cargo check
```

The `wgpu-hal`/`naga` patches are required because the Horizon workspace patches
those crates; without them the standalone package resolves crates.io `wgpu-hal`
and fails on `disk_cache`.

---

## 8. Build and deploy on Ubuntu 24.04 x86_64

### On the build PC (Ubuntu 24.04 or WSL2 Ubuntu — not the server VM)

```sh
# 1. Toolchain
rustup toolchain install 1.99.0
rustup default 1.99.0

# 2. Horizon launcher (install from the Horizon release/launcher package)
#    The launcher must have a prepared base release.

# 3. Build the mod into a standalone server engine
horizon-launcher native-build "/path/to/horizon-auth" \
  --server-output "$HOME/horizon-engine" \
  --version nightly-2026-11-08 \
  --trust-native-code
```

The export folder contains `horizon-engine`, prepared resources,
`native-data/`, and `native-mods.json`.

### On the server (Ubuntu 24.04 x86_64)

```sh
# Copy the whole export, not just the executable.
sudo mkdir -p /opt/horizon-engine
sudo rsync -a "$HOME/horizon-engine/" /opt/horizon-engine/

# Create/point the server at the pinned engine
horizon-server --dir /srv/horizon init
horizon-server --dir /srv/horizon config set engine.path=/opt/horizon-engine/horizon-engine
horizon-server --dir /srv/horizon restart
```

Horizon’s documented key is **`engine.path`** (docs/native-mods.md,
“Standalone server export”); the prompt’s “`[engine] path`” means the same
setting, but the actual `server.toml` key is `engine.path`.

### systemd unit

```ini
# /etc/systemd/system/horizon.service
[Unit]
Description=Horizon Minecraft server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=horizon
Group=horizon
WorkingDirectory=/srv/horizon
Environment=HORIZON_NO_AUDIO=1
ExecStart=/usr/local/bin/horizon-server --dir /srv/horizon run
Restart=on-failure
RestartSec=5
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now horizon
sudo journalctl -u horizon -f
```

### RAM impact

- Not measured on a real server in this session, so no fabricated number.
- The mod’s own static footprint is small: one SQLite file, one JSON config, a
  few in-memory maps, and no per-tick allocations for authenticated players.
- The real transient cost is **scrypt: 32 MiB per concurrent hash**. With the
  default `max_pending_per_ip = 3`, a single address can force at most ~96 MiB
  of hashing at once. Reduce `scrypt_log_n` to 14 (16 MiB) or 13 (8 MiB) on a
  small VPS.
- The engine’s own RAM is unchanged by this mod beyond that.

---

## 9. Known gaps / next steps

1. Build the engine export on Linux with the launcher.
2. Run the real test matrix: new player, returning player, wrong password ×3,
   timeout, reconnect, two players from one IP, operator bypass off.
3. Inspect the actual runtime paths for `config.json` / `accounts.db` and confirm
   `engine.path` in the installed `server.toml`.
4. Tune `holding` to a point inside the server’s world border and away from any
   build; the default `100000, 300, 100000` may be outside a custom border.
5. If the server uses a proxy, verify `Host::address` (API 407) returns the
   forwarded address; otherwise adjust per-IP limits.
6. Package the bundle as `.hnmod` through the launcher and record its SHA-256.

---

## 10. Citation index

- Trait mod structure: `docs/native-mods.md` §“Trait mods (draft)”.
- Standalone server export and `engine.path`: `docs/native-mods.md`
  §“Standalone server export”.
- Dependency rules / `Cargo.lock` allow list: `docs/native-mods.md`
  §“Dependencies come from Horizon’s `Cargo.lock`”; `crates/horizon-mod-deps/Cargo.toml`.
- Join / placement hooks:
  - `native_player_login` (API 172): `docs/native-mods.md` line 1341.
  - `native_player_logged_in` (API 57): `docs/native-mods.md` line 888 and the
    function docs in `crates/horizon-client/src/game/native_hooks.rs:8641`.
  - `native_player_spawn_location`: `native_hooks.rs:7142`.
- Movement: `native_player_move`: `docs/native-mods.md` line 2840.
- Menus: `docs/native-mods.md` §“Server menus” (API 10).
- Chat: `native_player_chat` (API 399): `docs/native-mods.md` line 1801.
- Commands: `native_player_command` (API 95): `native_hooks.rs:4603`.
- Player tick: `native_player_tick` (API 31): `native_hooks.rs:2679`.
- Vanishing: `native_hide_player` (API 314): `docs/native-mods.md` line 1627.
- Kick: `docs/native-mods.md` line 3210.
- Address: `native-mods.md` line 1817 (API 407).
- Set block: API 24, `docs/native-mods.md` line 267.
- Config dir: API 26, `docs/native-mods.md` line 279.
- `.hnmod` layout: `docs/api/mod-files.md`.
- Server API / `server.toml`: `docs/api/server.md`.

All hook names and fields above were verified against
`crates/horizon-client/src/game/native_hooks.rs`,
`crates/horizon-client/src/game/native_menus.rs`,
`native-hooks.json`, and the generated `mod_api_generated*.rs` for commit
`084b20d5cfbb104d4a70f61091198831074619f1`.
