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
use eldenring::{
    cs::{BlockId, TalkScript},
    ez_state::EzStateValue,
};

pub const GAME_MENU_MIRROR: i32 = 1;
pub const GAME_MENU_LEVEL_UP: i32 = 2;
pub const GAME_MENU_REBIRTH: i32 = 3;

const OPEN_SOUL: i32 = 31;
const OPEN_CHARA_MAKE_MENU: i32 = 81;
const OPEN_REALLOCATE_ATTRIBUTES: i32 = 113;
/// Any talk id: the commands above don't read it.
const TALK_ID: i32 = 1000;

/// Pending request (GAME_MENU_*), 0 = none.
static REQUEST: AtomicI32 = AtomicI32::new(0);
/// Result of the last request: 0 none yet, 1 opened, 2 no character loaded,
/// 3 the game refused the command (see EzStateInvokeError).
static LAST_RESULT: AtomicI32 = AtomicI32::new(0);
static INSTALL: Once = Once::new();
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
    // The talk-script vtables come from the bindings' address tables.
    if supported_version(Program::current()).is_none() {
        return 0;
    }
    INSTALL.call_once(|| {
        let Ok(task_imp) = (unsafe { CSTaskImp::instance() }) else {
            return;
        };
        let task = task_imp.run_recurring(open_game_menu_task as fn(&FD4TaskData), CSTaskGroupIndex::FrameBegin);
        std::mem::forget(task);
        INSTALLED.store(true, Ordering::Release);
    });
    if !INSTALLED.load(Ordering::Acquire) {
        return 0;
    }
    REQUEST.store(kind, Ordering::Release);
    1
}

/// Outcome of the last request, for the Hub's log.
#[unsafe(no_mangle)]
pub extern "C" fn nt_game_menu_last_result() -> i32 {
    LAST_RESULT.load(Ordering::Acquire)
}

fn open_game_menu_task(_: &FD4TaskData) {
    let kind = REQUEST.swap(0, Ordering::AcqRel);
    if kind == 0 {
        return;
    }
    // A panic must not unwind into the game's task loop.
    if std::panic::catch_unwind(|| open_game_menu(kind)).is_err() {
        LAST_RESULT.store(3, Ordering::Release);
    }
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
    let handle = player.chr_ins.field_ins_handle;
    let Ok(mut opener) = OPENER.lock() else {
        return;
    };
    let script = opener
        .0
        .get_or_insert_with(|| Box::new(TalkScript::new(BlockId::none(), TALK_ID, handle)));
    script.npc_talk.base.field_ins_handle = handle;
    let result = match kind {
        GAME_MENU_MIRROR => script.event((OPEN_CHARA_MAKE_MENU, [EzStateValue::Int32(1)])),
        GAME_MENU_LEVEL_UP => script.event(OPEN_SOUL),
        GAME_MENU_REBIRTH => script.event(OPEN_REALLOCATE_ATTRIBUTES),
        _ => return,
    };
    LAST_RESULT.store(if result.is_ok() { 1 } else { 3 }, Ordering::Release);
    log_line(format_args!("game menu {kind}: {result:?}"));
}
