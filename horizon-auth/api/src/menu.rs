//! The inventory-based login screen. It is a vanilla chest menu
//! (`generic_9x3`) built from ordinary item stacks, so a stock 26.3 client
//! renders it: no client mod is needed (docs/native-mods.md, "Server menus",
//! API 10). Every click is a no-op in the menu engine and reaches
//! `Hooks::menu_clicked`, which is exactly what a button needs.
use horizon_client::mod_api::{ItemStack, Text};
use serde_json::json;

pub const ROWS: u8 = 3;
pub const SIZE: usize = 27;

pub const SLOT_INFO: usize = 4;
pub const SLOT_REGISTER: usize = 11;
pub const SLOT_LOGIN: usize = 13;
pub const SLOT_CHANGE: usize = 15;
pub const SLOT_LEAVE: usize = 22;

pub fn title() -> Text {
    Text::legacy("\u{a7}8Horizon \u{a7}7Auth")
}

/// `registered` selects which of Register/Login is the active button. The
/// inactive one is still shown, greyed out, so the player can see both paths.
pub fn items(registered: bool, attempts: u32, max_attempts: u32) -> Vec<Option<ItemStack>> {
    let mut slots = vec![None; SIZE];

    for i in 0..SIZE {
        let border = i < 9 || i >= 18 || i % 9 == 0 || i % 9 == 8;
        if border {
            slots[i] = Some(pane());
        }
    }

    slots[SLOT_INFO] = Some(info(registered, attempts, max_attempts));

    slots[SLOT_REGISTER] = Some(if registered {
        disabled(
            "minecraft:gray_dye",
            "Already registered",
            &["Use Login below.", "One account per name."],
        )
    } else {
        button(
            "minecraft:lime_dye",
            "Register",
            &[
                "Click here, then type:",
                "/register <password> <password>",
                "Passwords are hashed, never stored.",
            ],
        )
    });

    slots[SLOT_LOGIN] = Some(if registered {
        button(
            "minecraft:paper",
            "Login",
            &["Click here, then type:", "/login <password>"],
        )
    } else {
        disabled(
            "minecraft:gray_dye",
            "No account yet",
            &["Register first with /register.", "Then log in with /login."],
        )
    });

    slots[SLOT_CHANGE] = Some(button(
        "minecraft:book",
        "Change password",
        &[
            "After login, type:",
            "/changepassword <old> <new>",
            "or /changepassword <new> <new>",
        ],
    ));

    slots[SLOT_LEAVE] = Some(button(
        "minecraft:barrier",
        "Disconnect",
        &["Close the server connection", "and come back later."],
    ));

    slots
}

fn info(registered: bool, attempts: u32, max_attempts: u32) -> ItemStack {
    let mut stack = ItemStack::new("minecraft:knowledge_book", 1);
    let (status, colour) = if registered {
        ("Account found", "green")
    } else {
        ("New player", "yellow")
    };
    name(
        &mut stack,
        &json!({"text": "Login required", "color": "gold", "bold": true}),
    );
    let attempts_line = if attempts == 0 {
        "No failed attempts this session.".to_owned()
    } else {
        format!("Failed attempts: {attempts}/{max_attempts}.")
    };
    lore(
        &mut stack,
        &[
            json!({"text": status, "color": colour}),
            json!({"text": "Type /login <password>", "color": "white"}),
            json!({"text": "or /register <password> <password>", "color": "white"}),
            json!({"text": attempts_line, "color": "gray"}),
            json!({"text": "Chat and movement are locked until you log in.", "color": "dark_gray"}),
        ],
    );
    stack
}

fn button(item: &str, label: &str, lines: &[&str]) -> ItemStack {
    let mut stack = ItemStack::new(item, 1);
    name(
        &mut stack,
        &json!({"text": label, "color": "green", "bold": true}),
    );
    let lore_lines: Vec<_> = lines
        .iter()
        .map(|line| json!({"text": line, "color": "gray"}))
        .collect();
    lore(&mut stack, &lore_lines);
    stack
}

fn disabled(item: &str, label: &str, lines: &[&str]) -> ItemStack {
    let mut stack = ItemStack::new(item, 1);
    name(
        &mut stack,
        &json!({"text": label, "color": "dark_gray", "bold": true}),
    );
    let lore_lines: Vec<_> = lines
        .iter()
        .map(|line| json!({"text": line, "color": "dark_gray"}))
        .collect();
    lore(&mut stack, &lore_lines);
    stack
}

fn pane() -> ItemStack {
    let mut stack = ItemStack::new("minecraft:black_stained_glass_pane", 1);
    name(&mut stack, &json!({"text": " "}));
    stack
}

fn name(stack: &mut ItemStack, component: &serde_json::Value) {
    stack
        .data_mut()
        .set_loot_name("minecraft:custom_name", component);
}

fn lore(stack: &mut ItemStack, lines: &[serde_json::Value]) {
    stack
        .data_mut()
        .set_loot_component("minecraft:lore", &serde_json::Value::Array(lines.to_vec()));
}
