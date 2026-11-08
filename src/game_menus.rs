//! NativeTweaks: opens the mirror (appearance) and rebirth screens from the
//! Hub with the talk-script commands the game's NPCs use. The command ids come
//! from CSEzStateTalkEvent's dispatcher (jump table of 0x9A commands), the same
//! in WW 2.7.1.0 and 2.7.2.0:
//!  - 81(1) -> menu type 30 (CharaMakeMenu); 81(0) opens type 16 (CharaMakeMenu2),
//!    which looked the same in the in-game test, so only 81(1) is used;
//!  - 31() -> menu type 10, the level-up screen Melina opens at a grace
//!    (OpenSoul; same id as in soulstruct's talk command list);
//!  - 113() -> menu type 19 (ReallocateAttributes), Rennala's rebirth. The
//!    Larval Tear is checked and taken by her talk script, not by the command,
//!    so the screen opens for free.
//! Built into NativeTweaks.dll whether or not Multiple Spirit Ashes is active.

use super::*;
use std::sync::atomic::AtomicUsize;
use eldenring::{
    cs::{BlockId, TalkScript},
    ez_state::EzStateValue,
};

pub const GAME_MENU_MIRROR: i32 = 1;
pub const GAME_MENU_LEVEL_UP: i32 = 2;
pub const GAME_MENU_REBIRTH: i32 = 3;
/// A shop over ShopLineupParam rows SHOP_BEGIN..=SHOP_END (the Hub's item
/// shop; talk command 22, OpenRegularShop, as fromsoftware-rs' invoke-esd).
pub const GAME_MENU_SHOP: i32 = 4;

const OPEN_SOUL: i32 = 31;
const OPEN_CHARA_MAKE_MENU: i32 = 81;
const OPEN_REALLOCATE_ATTRIBUTES: i32 = 113;
const OPEN_REGULAR_SHOP: i32 = 22;
/// begin << 32 | end, so a request never mixes two ranges.
static SHOP_RANGE: AtomicU64 = AtomicU64::new(0);
/// Frames a shop request has waited for the menu before it (F11) to close.
static SHOP_WAIT: AtomicI32 = AtomicI32::new(0);
/// The top in-game menu job, published by the frame task every frame so the
/// Hub never reads menu memory from its own thread (audit 2026-11-08).
static TOP_MENU_JOB: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Frames to wait before the shop opens, so a talk menu that queued it (the
/// ERNativeUI F11 list) has closed first.
static SHOP_DELAY: AtomicI32 = AtomicI32::new(0);
/// Any talk id: the commands above don't read it.
const TALK_ID: i32 = 1000;

/// Pending request (GAME_MENU_*), 0 = none.
static REQUEST: AtomicI32 = AtomicI32::new(0);
/// Result of the last request: 0 none yet, 1 opened, 2 no character loaded,
/// 3 the game refused the command (see EzStateInvokeError), 4 another menu
/// stayed open (shop), 5 the player is dead, 6 other players in the world.
static LAST_RESULT: AtomicI32 = AtomicI32::new(0);
static INSTALL: Mutex<()> = Mutex::new(());
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The talk script lives as long as the game: the open menu keeps pointers to
/// its NPC menu state until it closes.
struct Opener(Option<Box<TalkScript>>);
// Only touched from the frame task.
unsafe impl Send for Opener {}
static OPENER: Mutex<Opener> = Mutex::new(Opener(None));

/// Queues a screen to open on the next game frame. Returns 1 when queued, 0
/// for an unknown kind or when the game's task scheduler isn't there yet.
#[unsafe(no_mangle)]
pub extern "C" fn nt_open_game_menu(kind: i32) -> u8 {
    if kind != GAME_MENU_MIRROR && kind != GAME_MENU_LEVEL_UP && kind != GAME_MENU_REBIRTH {
        return 0;
    }
    queue_game_menu(kind)
}

/// Queues the item shop over ShopLineupParam rows begin..=end (same frame
/// task as the other screens).
#[unsafe(no_mangle)]
pub extern "C" fn nt_open_shop(begin: i32, end: i32) -> u8 {
    if begin <= 0 || end < begin {
        return 0;
    }
    SHOP_RANGE.store(((begin as u32 as u64) << 32) | end as u32 as u64, Ordering::Release);
    SHOP_DELAY.store(4, Ordering::Release);
    SHOP_WAIT.store(0, Ordering::Release);
    queue_game_menu(GAME_MENU_SHOP)
}

fn queue_game_menu(kind: i32) -> u8 {
    // The talk-script vtables come from the bindings' address tables.
    if supported_version(Program::current()).is_none() {
        return 0;
    }
    crate::game_menus::install_frame_task(&INSTALL, &INSTALLED, open_game_menu_task);
    if !INSTALLED.load(Ordering::Acquire) {
        return 0;
    }
    REQUEST.store(kind, Ordering::Release);
    1
}

/// Registers a FrameBegin task once. A try before the game's task manager
/// exists does not use up the attempt, so the caller can ask again (audit
/// 2026-11-08: with Once, one early failure disabled it for the session).
pub(crate) fn install_frame_task(lock: &Mutex<()>, installed: &AtomicBool, task: fn(&FD4TaskData)) {
    if installed.load(Ordering::Acquire) {
        return;
    }
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if installed.load(Ordering::Acquire) {
        return;
    }
    let Ok(task_imp) = (unsafe { CSTaskImp::instance() }) else {
        return;
    };
    let handle = task_imp.run_recurring(task, CSTaskGroupIndex::FrameBegin);
    std::mem::forget(handle);
    installed.store(true, Ordering::Release);
}

/// The main player's flasks, for F11 > Item Shop > Recover flasks: out[0] the
/// Flask of Crimson Tears row (full or empty, any level) or -1, out[1] the
/// Flask of Cerulean Tears, out[2] the Flask of Wondrous Physick, out[3] and
/// out[4] the HP / FP flask allotment the character keeps. Returns 0 when no
/// character is loaded. Read on the game thread (the talk menu callbacks).
#[unsafe(no_mangle)]
pub extern "C" fn nt_flask_state(out: *mut i32) -> u8 {
    if out.is_null() {
        return 0;
    }
    match std::panic::catch_unwind(flask_state) {
        Ok(Some(state)) => {
            unsafe { std::ptr::copy_nonoverlapping(state.as_ptr(), out, state.len()) };
            1
        }
        _ => 0,
    }
}

fn flask_state() -> Option<[i32; 5]> {
    let world = unsafe { WorldChrMan::instance() }.ok()?;
    let player = world.main_player.as_ref()?;
    let data = unsafe { player.player_game_data.as_ref() };
    let mut state = [-1, -1, -1, i32::from(data.max_hp_flask), i32::from(data.max_fp_flask)];
    for entry in data.equipment.equip_inventory_data.items_data.items() {
        if entry.item_id.category() != ItemCategory::Goods {
            continue;
        }
        let id = entry.item_id.param_id() as i32;
        match id {
            1000..=1025 => state[0] = id,
            1050..=1075 => state[1] = id,
            250 | 251 => state[2] = id,
            _ => {}
        }
    }
    Some(state)
}

/// Outcome of the last request, for the Hub's log.
#[unsafe(no_mangle)]
pub extern "C" fn nt_game_menu_last_result() -> i32 {
    LAST_RESULT.load(Ordering::Acquire)
}

fn open_game_menu_task(_: &FD4TaskData) {
    let top = std::panic::catch_unwind(read_top_menu_job).unwrap_or(usize::MAX);
    TOP_MENU_JOB.store(top, Ordering::Release);
    let kind = REQUEST.swap(0, Ordering::AcqRel);
    if kind == 0 {
        return;
    }
    if kind == GAME_MENU_SHOP {
        // Four frames for the F11 list to close, then wait (up to about five
        // seconds) until no other menu is open.
        let waiting = SHOP_DELAY.fetch_sub(1, Ordering::AcqRel) > 1
            || (top != 0 && top != usize::MAX && SHOP_WAIT.fetch_add(1, Ordering::AcqRel) < 300);
        if waiting {
            // keep it queued unless a newer request replaced it meanwhile
            let _ = REQUEST.compare_exchange(0, kind, Ordering::AcqRel, Ordering::Acquire);
            return;
        }
        if top != 0 && top != usize::MAX {
            LAST_RESULT.store(4, Ordering::Release);
            log_line(format_args!("game menu {kind}: another menu stayed open; not opened"));
            return;
        }
    }
    // A panic must not unwind into the game's task loop.
    if std::panic::catch_unwind(|| open_game_menu(kind)).is_err() {
        LAST_RESULT.store(3, Ordering::Release);
    }
}

/// The item shop opens sorted by item type, which groups items with a gap
/// between groups as the inventory does. Opening the shop sets the
/// "all items" sort (GameDataMan +0x60 = CSMenuSystemSaveLoad, +0x1440
/// sorts[20], index 12) to the game's default, so it is set right after the
/// open command, in the same frame, before the shop builds its list
/// (r157-test15: written earlier, it was overwritten with 0x80000068).
/// 0x80005141 = item type, ascending (the value Glorious Merchant, MIT,
/// documents). It stays the player's saved sort for that list.
fn set_shop_sort_by_type() {
    const MENU_SAVE_LOAD: usize = 0x60;
    const ALL_ITEMS_SORT: usize = 0x1440 + 12 * 4;
    const ITEM_TYPE_ASCENDING: u32 = 0x8000_5141;
    let Ok(game_data) = (unsafe { eldenring::cs::GameDataMan::instance() }) else {
        return;
    };
    let base = game_data as *const _ as *const u8;
    let menu = unsafe { base.add(MENU_SAVE_LOAD).cast::<usize>().read_unaligned() };
    if menu == 0 {
        return;
    }
    let sort = (menu + ALL_ITEMS_SORT) as *mut u32;
    let before = unsafe { sort.read_unaligned() };
    unsafe { sort.write_unaligned(ITEM_TYPE_ASCENDING) };
    log_line(format_args!("item shop sort: {before:#010x} -> {ITEM_TYPE_ASCENDING:#010x}"));
}

/// The game's top in-game menu job (CSPopupMenu +0xB0, as the ERNativeUI
/// talk menu reads it): 0 when no menu is open, usize::MAX when unknown. The
/// Hub polls it to put the armour shop's set layout back once it closes; the
/// value comes from the frame task (game thread), not from the Hub's thread.
#[unsafe(no_mangle)]
pub extern "C" fn nt_top_menu_job() -> usize {
    if !INSTALLED.load(Ordering::Acquire) {
        return usize::MAX;
    }
    TOP_MENU_JOB.load(Ordering::Acquire)
}

fn read_top_menu_job() -> usize {
    let Ok(man) = (unsafe { eldenring::cs::CSMenuManImp::instance() }) else {
        return usize::MAX;
    };
    let Some(popup) = man.popup_menu else {
        return usize::MAX;
    };
    unsafe { (popup.as_ptr() as *const u8).add(0xB0).cast::<usize>().read_unaligned() }
}

fn open_game_menu(kind: i32) {
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance() }) else {
        LAST_RESULT.store(2, Ordering::Release);
        return;
    };
    let Some(player) = world_chr_man.main_player.as_ref() else {
        LAST_RESULT.store(2, Ordering::Release);
        return;
    };
    // Not while dead (the menu would run over the death sequence), and the
    // mirror, level up and rebirth not with other players in the world, as
    // the game itself never offers them then (audit 2026-11-08).
    if player.chr_ins.modules.data.hp <= 0 {
        LAST_RESULT.store(5, Ordering::Release);
        log_line(format_args!("game menu {kind}: the player is dead; not opened"));
        return;
    }
    if kind != GAME_MENU_SHOP && world_chr_man.player_chr_set.characters().count() > 1 {
        LAST_RESULT.store(6, Ordering::Release);
        log_line(format_args!("game menu {kind}: other players are in the world; not opened"));
        return;
    }
    let handle = player.chr_ins.field_ins_handle;
    // a panic while it was held must not disable the menus for good
    let mut opener = OPENER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let script = opener
        .0
        .get_or_insert_with(|| Box::new(TalkScript::new(BlockId::none(), TALK_ID, handle)));
    script.npc_talk.base.field_ins_handle = handle;
    let result = match kind {
        GAME_MENU_MIRROR => script.event((OPEN_CHARA_MAKE_MENU, [EzStateValue::Int32(1)])),
        GAME_MENU_LEVEL_UP => script.event(OPEN_SOUL),
        GAME_MENU_REBIRTH => script.event(OPEN_REALLOCATE_ATTRIBUTES),
        GAME_MENU_SHOP => {
            let range = SHOP_RANGE.load(Ordering::Acquire);
            script.event((
                OPEN_REGULAR_SHOP,
                [EzStateValue::Int32((range >> 32) as u32 as i32), EzStateValue::Int32(range as u32 as i32)],
            ))
        }
        _ => return,
    };
    if kind == GAME_MENU_SHOP && result.is_ok() {
        set_shop_sort_by_type();
    }
    LAST_RESULT.store(if result.is_ok() { 1 } else { 3 }, Ordering::Release);
    log_line(format_args!("game menu {kind}: {result:?}"));
}
