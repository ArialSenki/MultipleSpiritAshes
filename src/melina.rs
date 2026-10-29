//! NativeTweaks: Melina as a spirit companion (Melina companion, phase 2).
//! The game has a fighting Melina it never uses: model c2180 with 165
//! animations (31 attacks in 3xxx) and 36 attacks in AtkParam_Npc
//! (2180xxx). A spirit ash is a BuddyParam row whose triggerSpEffectId,
//! put on the player, summons npcParamId with npcThinkParamId's AI; the model
//! comes from the NpcParam id ((id / 10000) % 10000: Tiche 121000000 ->
//! c2100, Melina's 21809000 -> c2180). Confirmed in game 2026-10-28.
//!  - BuddyParam 33000000 (trigger 20230000, no character: one of the empty
//!    rows the game leaves, 32300000-33000000) becomes a copy of Black Knife
//!    Tiche's 20000000 (her AI and appearing) with Melina's NpcParam
//!    21809000. Tried and dropped in game (2026-10-28): her story AI's
//!    battle goal 218000 (she didn't attack) and her animation 20000 as
//!    the appearing one (it is the accord scene: she ends up sitting).
//!  - NpcParam 21809000 (a hostile Melina, team 6) gets the spirit team 47
//!    and Tiche's spirit SpEffects (spEffectID0/4/6/7/8/10).
//! Companion (Hub option MelinaCompanion): while it is on she is summoned
//! again whenever she is not with you, except while you rest at a grace
//! (player animations 68000-68199), for 1.5 s after you get up, or while you
//! are dead. When you sit down she sits beside you (20000, the accord
//! scene's pose); when you get up she goes back to her idle (0) and follows.
//! Her TAE lacks Tiche's appear/leave (1830/1840) and fast run (2300), so she
//! appeared without effect and fell behind walking: she gets the spirit fade
//! (SpEffect 16316, as MSA's dismissal) when she appears, and when she stays
//! over 12 m away for 3 s the game's own buddy warp brings her over (her
//! entry in SummonBuddyWarpManager set to stage 1, as MSA's stuck-Ash warp).
//! Out of combat the spirit AI's following never steered her (2026-10-28: always
//! animation 2000 straight ahead, even with turning restored by the Hub's
//! SetTurnSpeed hook; writing her orientation each frame only gave a short
//! forced turn). So out of combat (no attack, dodge or hit for 3 s) she is
//! kept in her idle (0) instead of walking, and when you get over 8 m away
//! for 1 s the warp brings her beside you, like the Melina of the story who
//! appears by your side. In a fight her AI moves her and the warp waits 12 m.
//! Rows are changed the first time she is summoned and stay so until the game
//! closes. Built into NativeTweaks.dll whether or not Multiple Spirit Ashes is
//! active (summoning away from a monument needs its Summon anywhere).

use super::*;
use eldenring::cs::{BuddyParam, NpcParam, NpcThinkParam};

const MELINA_BUDDY: u32 = 33000000;
const MELINA_TRIGGER: i32 = 20230000;
const MELINA_NPC: u32 = 21809000;
const TICHE_BUDDY: u32 = 20000000;
const TICHE_NPC: u32 = 121000000;
const SPIRIT_TEAM: u8 = 47;
const REST_ANIMATION: i32 = 20000;
const IDLE_ANIMATION: i32 = 0;
const FADE_SP_EFFECT_ID: i32 = 16316;
/// Her own AI row: Tiche's spirit AI with a short memory, so a walking Melina
/// doesn't keep marching after a target you left (test 2026-10-28). The
/// row belongs to the empty BuddyParam 31800000 (no trigger), unused.
const MELINA_THINK: u32 = 150000001;
const SIGHT_FORGET_SECONDS: f32 = 4.0;
const BACK_HOME_BATTLE_DIST: u16 = 6;
const CATCH_UP_DISTANCE: f32 = 12.0;
const CATCH_UP_WAIT: Duration = Duration::from_secs(3);
const CATCH_UP_COOLDOWN: Duration = Duration::from_secs(6);
const CALM_CATCH_UP_DISTANCE: f32 = 8.0;
const CALM_CATCH_UP_WAIT: Duration = Duration::from_secs(1);
const CALM_CATCH_UP_COOLDOWN: Duration = Duration::from_secs(3);
const REPORT_INTERVAL: Duration = Duration::from_secs(3);
const PEACE_AFTER_FIGHT: Duration = Duration::from_secs(3);
/// Her walks (2000-2003; 2100 runs only in a fight).
const WALK_ANIMATIONS: std::ops::RangeInclusive<i32> = 2000..=2003;
/// Player grace animations: sit down, rest, level up, stand up (c0000 a00).
const GRACE_ANIMATIONS: std::ops::Range<i32> = 68000..68200;
const AFTER_REST: Duration = Duration::from_millis(1500);
const SUMMON_INTERVAL: Duration = Duration::from_secs(5);

static REQUESTED: AtomicBool = AtomicBool::new(false);
static COMPANION: AtomicBool = AtomicBool::new(false);
static ROWS_READY: AtomicBool = AtomicBool::new(false);
static INSTALL: Once = Once::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);

struct CompanionState {
    resting: bool,
    rest_animation_sent: bool,
    stood_up_at: Option<Instant>,
    last_summon: Option<Instant>,
    /// Her character last frame (0 = none), to give a new one the fade.
    seen: usize,
    far_since: Option<Instant>,
    last_warp: Option<Instant>,
    /// Diagnostics: last line about her, and the other spirits already logged.
    last_report: Option<Instant>,
    reported_buddies: Vec<i32>,
    /// Last frame she attacked, dodged or was hit.
    last_fight: Option<Instant>,

}

static STATE: Mutex<CompanionState> = Mutex::new(CompanionState {
    resting: false,
    rest_animation_sent: false,
    stood_up_at: None,
    last_summon: None,
    seen: 0,
    far_since: None,
    last_warp: None,
    last_report: None,
    reported_buddies: Vec::new(),
    last_fight: None,

});

fn install() -> bool {
    INSTALL.call_once(|| {
        let Ok(task_imp) = (unsafe { CSTaskImp::instance() }) else {
            return;
        };
        let task = task_imp.run_recurring(melina_task as fn(&FD4TaskData), CSTaskGroupIndex::FrameBegin);
        std::mem::forget(task);
        INSTALLED.store(true, Ordering::Release);
    });
    INSTALLED.load(Ordering::Acquire)
}

/// Summons Melina on the next game frame. Returns 1 when queued, 0 when the
/// game's task scheduler isn't there yet.
#[unsafe(no_mangle)]
pub extern "C" fn nt_summon_melina() -> u8 {
    if !install() {
        return 0;
    }
    REQUESTED.store(true, Ordering::Release);
    1
}

/// Turns the companion on or off. Returns 1 when the frame task runs, 0 when
/// the game's task scheduler isn't there yet (the Hub asks again).
#[unsafe(no_mangle)]
pub extern "C" fn nt_set_melina_companion(enabled: u8) -> u8 {
    COMPANION.store(enabled != 0, Ordering::Release);
    if enabled == 0 {
        return 1;
    }
    u8::from(install())
}

fn melina_task(_: &FD4TaskData) {
    // A panic must not unwind into the game's task loop.
    let _ = std::panic::catch_unwind(update_melina);
}

fn update_melina() {
    let requested = REQUESTED.swap(false, Ordering::AcqRel);
    let companion = COMPANION.load(Ordering::Acquire);
    if !requested && !companion {
        return;
    }
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let melina = live_melina(world_chr_man);
    look_after(world_chr_man, melina, &mut state);
    let Some(player) = world_chr_man.main_player.as_mut() else {
        return;
    };
    let animation = current_animation(&player.chr_ins).0 % 1_000_000;
    let resting = GRACE_ANIMATIONS.contains(&animation);
    if resting && !state.resting {
        state.rest_animation_sent = false;
        log_line(format_args!("melina: you rest at a grace (animation {animation})"));
    }
    if !resting && state.resting {
        state.stood_up_at = Some(Instant::now());
        if let Some(chr) = melina
            && state.rest_animation_sent
        {
            unsafe { (*chr).modules.as_mut().event.as_mut().request_animation_id = IDLE_ANIMATION };
            log_line(format_args!("melina: you got up, idle animation {IDLE_ANIMATION}"));
        }
    }
    state.resting = resting;
    if resting {
        if let Some(chr) = melina
            && !state.rest_animation_sent
        {
            // She sits beside the grace, as when she offers the accord.
            unsafe { (*chr).modules.as_mut().event.as_mut().request_animation_id = REST_ANIMATION };
            state.rest_animation_sent = true;
            log_line(format_args!("melina: rest animation {REST_ANIMATION}"));
        }
        if !requested {
            return;
        }
    }
    let wants_summon = requested
        || (companion
            && melina.is_none()
            && chr_alive(&player.chr_ins)
            && state.stood_up_at.is_none_or(|at| at.elapsed() >= AFTER_REST)
            && state.last_summon.is_none_or(|at| at.elapsed() >= SUMMON_INTERVAL));
    if !wants_summon || (melina.is_some() && !requested) {
        return;
    }
    if !ROWS_READY.load(Ordering::Acquire) && !prepare_rows() {
        log_line(format_args!("melina: rows not ready (BuddyParam or NpcParam missing)"));
        return;
    }
    player.apply_speffect(MELINA_TRIGGER, true);
    state.last_summon = Some(Instant::now());
    log_line(format_args!(
        "melina: summon trigger {MELINA_TRIGGER} applied ({})",
        if requested { "button" } else { "companion" }
    ));
}

/// A new Melina gets the spirit fade; one left behind is warped over.
fn look_after(world: &mut WorldChrMan, melina: Option<*mut ChrIns>, state: &mut CompanionState) {
    let Some(chr_ptr) = melina else {
        state.seen = 0;
        state.far_since = None;
        return;
    };
    let chr = unsafe { &mut *chr_ptr };
    if state.seen != chr_ptr as usize {
        state.seen = chr_ptr as usize;
        let think = chr.as_subclass_mut::<EnemyIns>().map_or(-1, |enemy| {
            enemy.apply_speffect(FADE_SP_EFFECT_ID, true);
            enemy.npc_think_param
        });
        log_line(format_args!(
            "melina: appeared, fade {FADE_SP_EFFECT_ID}; NpcParam {} think {think} team {} type {:?}",
            chr.npc_param_id, chr.team_type, chr.chr_type as i32
        ));
    }
    let distance = chr.distance_to_player_sqr.max(0.0).sqrt();
    if state.last_report.is_none_or(|at| at.elapsed() >= REPORT_INTERVAL) {
        state.last_report = Some(Instant::now());
        let (animation, _, _) = current_animation(chr);
        log_line(format_args!("melina: {distance:.1} m from you, animation {animation}"));
        report_other_spirits(world, state);
    }
    let calm = keep_calm(chr, state);
    let (reach, wait, cooldown) = if calm {
        (CALM_CATCH_UP_DISTANCE, CALM_CATCH_UP_WAIT, CALM_CATCH_UP_COOLDOWN)
    } else {
        (CATCH_UP_DISTANCE, CATCH_UP_WAIT, CATCH_UP_COOLDOWN)
    };
    if state.resting || distance <= reach {
        state.far_since = None;
        return;
    }
    let far_since = *state.far_since.get_or_insert_with(Instant::now);
    if far_since.elapsed() < wait || state.last_warp.is_some_and(|at| at.elapsed() < cooldown) {
        return;
    }
    let handle = chr.field_ins_handle;
    let Some(manager_ptr) = summon_buddy_manager_ptr(world) else {
        return;
    };
    let manager = unsafe { &mut *manager_ptr };
    let stage = manager
        .warp_manager
        .as_ref()
        .entries
        .iter()
        .find(|pair| pair.first == handle)
        .map(|pair| std::ptr::from_ref(&pair.second).cast::<u8>().cast_mut());
    let Some(stage) = stage else {
        return;
    };
    let before = unsafe { stage.read_volatile() };
    if before == 0 {
        unsafe { stage.write_volatile(1) };
    }
    state.last_warp = Some(Instant::now());
    state.far_since = None;
    log_line(format_args!("melina: {distance:.1} m behind, warp {}", if before == 0 { "raised" } else { "busy" }));
}

/// Out of combat she stays in her idle instead of walking off. Returns whether
/// she is calm (no attack, dodge or hit for PEACE_AFTER_FIGHT).
fn keep_calm(chr: &mut ChrIns, state: &mut CompanionState) -> bool {
    let animation = current_animation(chr).0 % 1_000_000;
    if (3000..10000).contains(&animation) {
        state.last_fight = Some(Instant::now());
    }
    let calm = state.last_fight.is_none_or(|at| at.elapsed() >= PEACE_AFTER_FIGHT);
    if calm && !state.resting && WALK_ANIMATIONS.contains(&animation) {
        chr.modules.as_mut().event.as_mut().request_animation_id = IDLE_ANIMATION;
    }
    calm
}

/// One line per other spirit kind summoned, with the AI it really runs, to
/// compare with Melina's.
fn report_other_spirits(world: &mut WorldChrMan, state: &mut CompanionState) {
    let Some(manager_ptr) = summon_buddy_manager_ptr(world) else {
        return;
    };
    let manager = unsafe { &mut *manager_ptr };
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            if group.is_remote || state.reported_buddies.contains(&group.buddy_param_id) {
                continue;
            }
            state.reported_buddies.push(group.buddy_param_id);
            let chr = unsafe { group.chr_ins.as_mut() };
            let think = chr.as_subclass_mut::<EnemyIns>().map_or(-1, |enemy| enemy.npc_think_param);
            log_line(format_args!(
                "melina: spirit BuddyParam {} NpcParam {} think {think} team {} follow {} share-off {}",
                group.buddy_param_id, chr.npc_param_id, chr.team_type, group.follow_type, group.disable_pc_target_share
            ));
        }
    }
}

/// Melina's character while she is summoned and not leaving.
fn live_melina(world: &mut WorldChrMan) -> Option<*mut ChrIns> {
    let manager = unsafe { &mut *summon_buddy_manager_ptr(world)? };
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            if group.is_remote || group.disappear_requested || group.buddy_param_id != MELINA_BUDDY as i32 {
                continue;
            }
            let chr = group.chr_ins.as_ptr();
            if chr_alive(unsafe { &*chr }) {
                return Some(chr);
            }
        }
    }
    None
}

fn prepare_rows() -> bool {
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return false;
    };
    let Some(tiche) = repository.get::<BuddyParam>(TICHE_BUDDY).cloned() else {
        return false;
    };
    let Some(tiche_npc) = repository.get::<NpcParam>(TICHE_NPC).cloned() else {
        return false;
    };
    let Some(tiche_think) = repository.get::<NpcThinkParam>(TICHE_NPC).cloned() else {
        return false;
    };
    {
        let Some(think) = repository.get_mut::<NpcThinkParam>(MELINA_THINK) else {
            return false;
        };
        *think = tiche_think;
        think.set_sight_target_forget_time(SIGHT_FORGET_SECONDS);
        think.set_backhome_battle_dist(BACK_HOME_BATTLE_DIST);
    }
    {
        let Some(buddy) = repository.get_mut::<BuddyParam>(MELINA_BUDDY) else {
            return false;
        };
        *buddy = tiche;
        buddy.set_trigger_sp_effect_id(MELINA_TRIGGER);
        buddy.set_npc_param_id(MELINA_NPC as i32);
        buddy.set_npc_think_param_id(MELINA_THINK as i32);
    }
    let Some(npc) = repository.get_mut::<NpcParam>(MELINA_NPC) else {
        return false;
    };
    npc.set_team_type(SPIRIT_TEAM);
    npc.set_sp_effect_id0(tiche_npc.sp_effect_id0());
    npc.set_sp_effect_id4(tiche_npc.sp_effect_id4());
    npc.set_sp_effect_id6(tiche_npc.sp_effect_id6());
    npc.set_sp_effect_id7(tiche_npc.sp_effect_id7());
    npc.set_sp_effect_id8(tiche_npc.sp_effect_id8());
    npc.set_sp_effect_id10(tiche_npc.sp_effect_id10());
    ROWS_READY.store(true, Ordering::Release);
    log_line(format_args!(
        "melina: BuddyParam {MELINA_BUDDY} -> NpcParam {MELINA_NPC} (team {SPIRIT_TEAM}, AI {MELINA_THINK}: Tiche's, forget {SIGHT_FORGET_SECONDS} s, back home {BACK_HOME_BATTLE_DIST} m)"
    ));
    true
}
