//! Area follow, ported from Multiple Spirit Ashes 0.1.160-0.1.162
//! (`project/src/lib.rs`, `update_native_warp_all` and the player jump
//! detection; tested in game on 2.7.1.0 as "5 warps held, smooth crossing").
//! Crossing a map block shifts the world origin, so the player's position
//! jumps by several metres in one pass (as do teleports, doors, fog walls and
//! lifts). The Ashes stay registered in the old block and only react near it.
//! After such a jump, SummonBuddyWarpManager+0x30 ("warp all", read by
//! 0x1404c2fd0 in 2.7.1.0) is raised once, so the game's own warp machine
//! (stage 1 -> 2 -> 3, with fade) brings every registered Ash over. It never
//! sets a group's warp_requested, which made GameMan remove the Ashes in
//! 0.1.124-0.1.127.
//! Stuck Ashes (0.1.161, simplified): an Ash farther than 25 m (100 m on
//! foot) that does not get 1.5 m closer for 4 s after the player travelled
//! (or is beyond 3x that distance) while the player has not just jumped has
//! its own native warp entry set to stage 1, the state "warp all" sets, so
//! the game warps only that Ash with its fade.
//! Fog gates (1.0.2): walking through a boss fog moves the player only a few
//! metres, so neither the jump nor the 25 m stuck rule fires and the Ashes
//! stay behind the wall. When the player's fog-gate animation ends, every
//! idle native warp entry is set to stage 1 (the "warp all" flag is not read
//! there), whatever the Ash's distance. While the game runs a boss fight
//! (GameDataMan boss_fight_active, field bosses and riding included) until
//! it has been over for 3 s, the game's own warp triggers are set out of
//! reach and the stuck and jump warps are off, and every Ash carries
//! BUDDY_NO_FOLLOW (297103), so the spirit AI fights where its target is
//! instead of returning to the player at 23 m (common_logic_func.lua). A fog
//! wall with no boss fight behind it changes nothing after the crossing.
//! Elsewhere on foot, while any Ash fought in the last 10 s (combat animation
//! 3000-9999) every Ash within 100 m carries BUDDY_NO_FOLLOW, and the game
//! warps an Ash only beyond 100 m, after 30 s out of sight or when stuck on
//! its path; riding Torrent brings back the game's own warps and the usual
//! following. Player event animations (60000-60999) are logged once each.
//! Switching the feature (or the mod) off puts the game's warp triggers back
//! and takes BUDDY_NO_FOLLOW off every Ash (release_area_follow).
//! Free fighting (nt_msa_set_free_fighting, INI FreeFighting) picks where
//! the Ashes fight without the leash: everywhere (default), only in boss
//! zones, or nowhere (the game's own following and warps; fog gates still
//! bring the Ashes through).

use super::*;

const PLAYER_POSITION_DISCONTINUITY: f32 = 8.0;
const WARP_MANAGER_WARP_ALL_OFFSET: usize = 0x30;
const NATIVE_WARP_ALL_SETTLE: Duration = Duration::from_millis(750);
const NATIVE_WARP_ALL_PENDING_WINDOW: Duration = Duration::from_secs(8);
const NATIVE_WARP_ALL_MIN_DISTANCE: f32 = 10.0;
const NATIVE_WARP_ALL_CHECK_DELAY: Duration = Duration::from_millis(4000);
const NATIVE_WARP_ALL_FAIL_DISTANCE: f32 = 25.0;
const NATIVE_WARP_ALL_BUSY_RECHECK: Duration = Duration::from_millis(2000);
const NATIVE_WARP_ALL_BUSY_WAIT_MAX: Duration = Duration::from_secs(20);
const NATIVE_WARP_ALL_INTERVAL: Duration = Duration::from_millis(1500);
const _: () = assert!(
    std::mem::offset_of!(
        eldenring::cs::SummonBuddyWarpManager,
        trigger_time_ray_block
    ) == 0x18
);

#[derive(Default)]
pub(crate) struct AreaFollowState {
    last_player_sample: Option<([f32; 3], Instant)>,
    pending_since: Option<Instant>,
    raised_at: Option<Instant>,
    check_at: Option<Instant>,
    last_at: Option<Instant>,
    raised_first_check: Option<Instant>,
    disabled: bool,
    count: u32,
    stuck: HashMap<usize, StuckTrace>,
    last_stuck_warp: Option<Instant>,
    stuck_warps: u32,
    /// Since when the player plays a fog-gate animation.
    fog_since: Option<Instant>,
    /// The pending warp all comes from a fog gate: no minimum distance.
    fog_crossing: bool,
    /// Last player event animation logged (60000-60999).
    last_event_animation: i32,
    /// Stage trace (and front placement) of a fog-gate warp.
    fog_trace: Option<FogTrace>,
    /// From a fog gate or a boss fight until the fight is over: no native
    /// warp to the player, no stuck warp, no warp all on a jump.
    boss_zone: Option<BossZone>,
    /// GameDataMan boss_fight_active as last seen.
    last_boss_fight: Option<bool>,
    /// BUDDY_NO_FOLLOW applications since it was last off everywhere (to see
    /// in the log whether the marker stays on or must be put back).
    no_follow_applied: u32,
    /// Ashes carrying BUDDY_NO_FOLLOW after the last pass.
    no_follow_active: usize,
    /// Until when the Ashes count as fighting (last combat animation of any
    /// Ash + grace).
    fight_until: Option<Instant>,
    /// Per Ash: last battle state logged.
    battle_logged: HashMap<usize, bool>,
    /// The player rides (Torrent): vanilla warps, no BUDDY_NO_FOLLOW.
    riding: bool,
    /// The game's warp triggers (ray block, distance, path stacked), read
    /// once and checked sane before anything is written.
    saved_triggers: Option<[f32; 3]>,
    /// Warp trigger profile written last.
    warp_profile: Option<WarpProfile>,
    /// Last spawn point seen, to learn which way the player's forward is.
    last_spawn_origin: Option<[f32; 3]>,
    /// Address of the SummonBuddyWarpManager the state belongs to: a new one
    /// (loading a save, quitting to the title) starts with the game's values.
    warp_manager_addr: Option<usize>,
}

struct FogTrace {
    start: Instant,
    stages: String,
    /// Entries (by address) whose destination was already looked at.
    placed: HashSet<usize>,
}

struct BossZone {
    since: Instant,
    /// Since when the game's boss fight flag is off (the zone ends after
    /// BOSS_FIGHT_END_GRACE, so a phase change does not end it).
    fight_off_since: Option<Instant>,
}

/// Native warp triggers (SummonBuddyWarpManager +0x18/+0x1c/+0x20).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WarpProfile {
    /// Boss zone: out of reach.
    Boss,
    /// On foot elsewhere: only far away (100 m) or long out of sight (30 s);
    /// stuck on its path (the game's 3 s) still warps, as at small doors.
    OnFoot,
    /// Riding: the game's own values, so the Ashes keep up.
    Riding,
}

struct StuckTrace {
    best_distance: f32,
    /// Where the player was when the wait started. An Ash is only left
    /// behind if the player travelled; a player fighting at range from one
    /// spot never pulls a fighting Ash over.
    player_anchor: [f32; 3],
    since: Instant,
    last_warp: Option<Instant>,
}

const STUCK_DISTANCE: f32 = 25.0;
/// Beyond this many times the stuck distance an Ash is warped even if the
/// player stood still.
const STUCK_FAR_FACTOR: f32 = 3.0;
const STUCK_PROGRESS_STEP: f32 = 1.5;
const STUCK_WAIT: Duration = Duration::from_millis(4000);
const STUCK_COOLDOWN: Duration = Duration::from_millis(5000);
const STUCK_PLAYER_TRAVEL: f32 = 8.0;
/// The player's walk through a fog gate (c0000 TAE a00 60060, confirmed in
/// game in the 1.0.2 tests).
const FOG_GATE_ANIMATIONS: [i32; 1] = [60060];
/// Shorter than this is not a fog crossing (an interrupted animation).
const FOG_GATE_MIN_PLAY: Duration = Duration::from_millis(300);
/// How long a fog-gate warp is traced in the log.
const FOG_TRACE_WINDOW: Duration = Duration::from_secs(8);
/// The Ashes land this far in front of the player, side by side.
const FOG_FRONT_DISTANCE: f32 = 3.0;
const FOG_FRONT_SPACING: f32 = 1.5;
/// The game's own destination must be this close to the player (same
/// coordinate space) and this level with them (flat ground) to move it.
const FOG_FRONT_MAX_GAME_DISTANCE: f32 = 15.0;
const FOG_FRONT_MAX_HEIGHT: f32 = 1.0;
/// Boss zone: the game warps an Ash beyond 33 m, out of sight for 5 s or
/// stuck on its path for 3 s (GameSystemCommonParam). Inside the arena those
/// limits are out of reach, so the Ashes move and fight freely.
const BOSS_ZONE_DIST_TO_PLAYER: f32 = 100_000.0;
const BOSS_ZONE_TIME_RAY_BLOCK: f32 = 100_000.0;
const BOSS_ZONE_TIME_PATH_STACKED: f32 = 100_000.0;
/// The boss zone ends once the boss fight flag has been off this long.
const BOSS_FIGHT_END_GRACE: Duration = Duration::from_secs(3);
/// PLAN_SP_EFFECT_BUDDY_NO_FOLLOW (ai_define.lua). In the common spirit
/// logic (_COMMON_SetBuddyBattleActLogic, common_logic_func.lua) a following
/// Ash in combat walks back to the player at 23 m and ignores enemies more
/// than 28 m from them; with this marker it always takes its battle goal.
/// A bare SpEffect row (no stats, no vfx) in 2.7.2.0.
const BUDDY_NO_FOLLOW_SP_EFFECT: i32 = 297103;
/// On foot outside a boss zone: the game warps an Ash to the player only
/// beyond this distance or after this long out of sight, and an Ash in
/// battle keeps BUDDY_NO_FOLLOW only within this distance.
const OPEN_WORLD_DIST_TO_PLAYER: f32 = 100.0;
const OPEN_WORLD_TIME_RAY_BLOCK: f32 = 30.0;
/// An Ash that already carries BUDDY_NO_FOLLOW keeps it up to this much
/// beyond OPEN_WORLD_DIST_TO_PLAYER (no on/off flicker at the edge).
const NO_FOLLOW_HYSTERESIS: f32 = 5.0;
/// The Ashes still count as fighting this long after the last combat
/// animation (attack, dodge, guard, hit) of any of them.
const BATTLE_GRACE: Duration = Duration::from_secs(10);
const WARP_MANAGER_TIME_RAY_BLOCK_OFFSET: usize = 0x18;
const WARP_MANAGER_DIST_TO_PLAYER_OFFSET: usize = 0x1c;
const WARP_MANAGER_TIME_PATH_STACKED_OFFSET: usize = 0x20;
const WARP_TRIGGER_OFFSETS: [usize; 3] = [
    WARP_MANAGER_TIME_RAY_BLOCK_OFFSET,
    WARP_MANAGER_DIST_TO_PLAYER_OFFSET,
    WARP_MANAGER_TIME_PATH_STACKED_OFFSET,
];
/// Sign of the player's +Z axis against the game's spawn point (in front of
/// the player): learnt at each summon, +1 until then.
static FORWARD_SIGN: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(1);

/// The player's +Z axis on the ground plane, from the XYZW quaternion.
fn horizontal_forward(q: [f32; 4]) -> Option<[f32; 2]> {
    let [x, y, z, w] = q;
    let fx = 2.0 * (x * z + w * y);
    let fz = 1.0 - 2.0 * (x * x + y * y);
    let length = (fx * fx + fz * fz).sqrt();
    (length > 0.1).then(|| [fx / length, fz / length])
}

/// Spot `index` of `count` in a row in front of the player.
fn front_spot(player: [f32; 3], forward: [f32; 2], index: usize, count: usize) -> [f32; 3] {
    let side = index as f32 - (count.max(1) - 1) as f32 / 2.0;
    let right = [forward[1], -forward[0]];
    [
        player[0] + forward[0] * FOG_FRONT_DISTANCE + right[0] * side * FOG_FRONT_SPACING,
        player[1],
        player[2] + forward[1] * FOG_FRONT_DISTANCE + right[1] * side * FOG_FRONT_SPACING,
    ]
}

fn is_fog_gate_animation(animation: i32) -> bool {
    FOG_GATE_ANIMATIONS.contains(&animation.rem_euclid(1_000_000))
}

/// Fog-gate state per pass: Some(play time) when the animation just ended
/// after playing long enough (a crossing).
fn fog_gate_step(
    fog_since: &mut Option<Instant>,
    animation: i32,
    now: Instant,
) -> Option<Duration> {
    if is_fog_gate_animation(animation) {
        fog_since.get_or_insert(now);
        return None;
    }
    let played = now.saturating_duration_since(fog_since.take()?);
    (played >= FOG_GATE_MIN_PLAY).then_some(played)
}

/// Free fighting: nowhere (the game's leash and warps).
pub const FREE_FIGHTING_OFF: u8 = 0;
/// Free fighting only in boss zones.
pub const FREE_FIGHTING_BOSSES: u8 = 1;
/// Free fighting in boss zones and in any fight on foot (default).
pub const FREE_FIGHTING_EVERYWHERE: u8 = 2;
static FREE_FIGHTING: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(FREE_FIGHTING_EVERYWHERE);

/// Where the Ashes fight without the leash: 0 nowhere, 1 boss zones only,
/// 2 boss zones and any fight on foot (default; larger values count as 2).
/// Takes effect on the next frame.
#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_set_free_fighting(mode: u8) {
    let mode = mode.min(FREE_FIGHTING_EVERYWHERE);
    let before = FREE_FIGHTING.swap(mode, Ordering::Relaxed);
    if before != mode {
        log_line(format_args!("free fighting: {before} -> {mode}"));
    }
}

/// (boss zones, fights elsewhere on foot) without the leash.
fn free_fighting() -> (bool, bool) {
    free_fighting_scopes(FREE_FIGHTING.load(Ordering::Relaxed))
}

fn free_fighting_scopes(mode: u8) -> (bool, bool) {
    (
        mode >= FREE_FIGHTING_BOSSES,
        mode >= FREE_FIGHTING_EVERYWHERE,
    )
}

static AREA_FOLLOW: LazyLock<Mutex<AreaFollowState>> =
    LazyLock::new(|| Mutex::new(AreaFollowState::default()));

fn distance_3d(left: [f32; 3], right: [f32; 3]) -> f32 {
    let dx = left[0] - right[0];
    let dy = left[1] - right[1];
    let dz = left[2] - right[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Per-frame entry point (post physics, feature on).
pub(crate) fn update_area_follow(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    dismissed_effects: &HashSet<i32>,
    player_position: Option<HavokPosition>,
    player_animation: Option<i32>,
    player_orientation: Option<[f32; 4]>,
    boss_fight: Option<bool>,
    player_riding: bool,
) {
    let Ok(mut state) = AREA_FOLLOW.lock() else {
        return;
    };
    let now = Instant::now();
    track_warp_manager(manager, &mut state);
    let jumped = detect_player_position_discontinuity(&mut state, player_position, now);
    // Inside a boss arena a jump is a block change under the player's feet
    // (two 39.9 m jumps mid-fight in the 1.0.2 test): no warp all there.
    // Same for a jump while Ashes fight far away (BUDDY_NO_FOLLOW on).
    if jumped && (state.boss_zone.is_some() || state.no_follow_active > 0) && !state.fog_crossing {
        state.pending_since = None;
        log_line(format_args!(
            "player jump ignored: {}",
            if state.boss_zone.is_some() {
                "inside the boss zone"
            } else {
                "Ashes in battle"
            }
        ));
    }
    if player_riding != state.riding {
        state.riding = player_riding;
        log_line(format_args!("player riding: {player_riding}"));
    }
    learn_forward_sign(manager, &mut state, player_position, player_orientation);
    if let Some(animation) = player_animation {
        detect_fog_gate(&mut state, animation, now);
    }
    update_boss_zone(&mut state, boss_fight, now);
    update_warp_profile(manager, &mut state);
    update_no_follow(
        manager,
        buddy_to_trigger,
        dismissed_effects,
        &mut state,
        player_position,
        now,
    );
    trace_fog_warp(
        manager,
        &mut state,
        player_position,
        player_orientation,
        now,
    );
    update_native_warp_all(
        manager,
        buddy_to_trigger,
        dismissed_effects,
        &mut state,
        player_position,
        now,
    );
    // A crossing is the warp-all's job; per-Ash warps wait until it is over.
    let crossing =
        state.pending_since.is_some() || state.raised_at.is_some() || state.check_at.is_some();
    if crossing || state.boss_zone.is_some() {
        state.stuck.clear();
    } else if let Some(player) = player_position {
        update_stuck_ashes(
            manager,
            buddy_to_trigger,
            dismissed_effects,
            &mut state,
            player,
            now,
        );
    }
}

fn update_stuck_ashes(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    dismissed_effects: &HashSet<i32>,
    state: &mut AreaFollowState,
    player: HavokPosition,
    now: Instant,
) {
    let mut present = HashSet::new();
    let mut warp = None;
    let player_here = [player.0, player.1, player.2];
    // On foot the Ashes may fight far away: only a really lost one is pulled.
    let stuck_distance = if state.riding || !free_fighting().1 {
        STUCK_DISTANCE
    } else {
        OPEN_WORLD_DIST_TO_PLAYER
    };
    let far_distance = stuck_distance * STUCK_FAR_FACTOR;
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                continue;
            };
            if group.is_remote || group.disappear_requested || dismissed_effects.contains(&effect) {
                continue;
            }
            let chr = unsafe { group.chr_ins.as_ref() };
            if !chr_alive(chr) {
                continue;
            }
            let key = std::ptr::from_ref(chr) as usize;
            let position = chr.modules.as_ref().physics.as_ref().position;
            let distance = distance_3d(
                [position.0, position.1, position.2],
                [player.0, player.1, player.2],
            );
            if distance < stuck_distance {
                state.stuck.remove(&key);
                continue;
            }
            present.insert(key);
            let trace = state.stuck.entry(key).or_insert(StuckTrace {
                best_distance: distance,
                player_anchor: player_here,
                since: now,
                last_warp: None,
            });
            // Getting closer restarts the wait (it is catching up).
            if distance < trace.best_distance - STUCK_PROGRESS_STEP {
                trace.best_distance = distance;
                if distance < far_distance {
                    trace.since = now;
                    trace.player_anchor = player_here;
                }
                continue;
            }
            let player_travelled =
                distance_3d(player_here, trace.player_anchor) >= STUCK_PLAYER_TRAVEL;
            if !player_travelled && distance < far_distance {
                continue;
            }
            let waited = now.saturating_duration_since(trace.since) >= STUCK_WAIT;
            let cooled = trace
                .last_warp
                .is_none_or(|last| now.saturating_duration_since(last) >= STUCK_COOLDOWN);
            if waited && cooled && warp.is_none() {
                warp = Some((key, chr.field_ins_handle, effect, distance));
            }
        }
    }
    state.stuck.retain(|key, _| present.contains(key));
    let Some((key, handle, effect, distance)) = warp else {
        return;
    };
    if state
        .last_stuck_warp
        .is_some_and(|last| now.saturating_duration_since(last) < NATIVE_WARP_ALL_INTERVAL)
    {
        return;
    }
    let stage_ptr = manager
        .warp_manager
        .as_ref()
        .entries
        .iter()
        .find(|pair| pair.first == handle)
        .map(|pair| std::ptr::from_ref(&pair.second).cast::<u8>().cast_mut());
    let stage_before = stage_ptr.map(|ptr| unsafe { ptr.read_volatile() });
    let raised = matches!(stage_before, Some(0));
    if let (true, Some(ptr)) = (raised, stage_ptr) {
        unsafe { ptr.write_volatile(1) };
        state.stuck_warps += 1;
    }
    state.last_stuck_warp = Some(now);
    if let Some(trace) = state.stuck.get_mut(&key) {
        trace.last_warp = Some(now);
        trace.since = now;
        trace.best_distance = distance;
        trace.player_anchor = player_here;
    }
    log_line(format_args!(
        "stuck Ash warp {}: n={} effect={effect} distance={distance:.1} stage_before={}",
        if raised { "raised" } else { "skipped" },
        state.stuck_warps,
        stage_before.map_or("missing".to_string(), |stage| stage.to_string())
    ));
}

/// A large player jump between two passes is either a teleport or a world
/// origin shift (the 0.1.148 log showed every Ash moving ~32 m in one pass at
/// a block change while distances stayed continuous).
fn detect_player_position_discontinuity(
    state: &mut AreaFollowState,
    player_position: Option<HavokPosition>,
    now: Instant,
) -> bool {
    let Some(position) = player_position else {
        return false;
    };
    let current = [position.0, position.1, position.2];
    let mut jumped = false;
    if let Some((previous, sampled_at)) = state.last_player_sample
        && now.saturating_duration_since(sampled_at) <= Duration::from_secs(1)
    {
        let jump = distance_3d(previous, current);
        if jump >= PLAYER_POSITION_DISCONTINUITY {
            log_line(format_args!(
                "player position discontinuity: jump={:.1} delta=({:.1},{:.1},{:.1}) dt_ms={}",
                jump,
                current[0] - previous[0],
                current[1] - previous[1],
                current[2] - previous[2],
                now.saturating_duration_since(sampled_at).as_millis(),
            ));
            state.pending_since = Some(now);
            jumped = true;
        }
    }
    state.last_player_sample = Some((current, now));
    jumped
}

/// The game puts a new spawn point in front of the player when an Ash is
/// summoned: that tells which sign of the player's +Z axis is "forward".
fn learn_forward_sign(
    manager: &SummonBuddyManager,
    state: &mut AreaFollowState,
    player_position: Option<HavokPosition>,
    player_orientation: Option<[f32; 4]>,
) {
    let origin = manager.spawn_origin;
    let origin = [origin.0, origin.1, origin.2];
    let changed = state
        .last_spawn_origin
        .is_none_or(|last| distance_3d(last, origin) > 0.05);
    state.last_spawn_origin = Some(origin);
    if !changed {
        return;
    }
    let (Some(player), Some(forward)) = (
        player_position,
        player_orientation.and_then(horizontal_forward),
    ) else {
        return;
    };
    let dx = origin[0] - player.0;
    let dz = origin[2] - player.2;
    let length = (dx * dx + dz * dz).sqrt();
    if !(1.0..=6.0).contains(&length) || (origin[1] - player.1).abs() > 2.0 {
        return;
    }
    let dot = (dx * forward[0] + dz * forward[1]) / length;
    if dot.abs() < 0.7 {
        log_line(format_args!(
            "spawn point direction: dot={dot:.2} (not straight ahead, sign kept) distance={length:.2}"
        ));
        return;
    }
    let sign = if dot > 0.0 { 1 } else { -1 };
    let before = FORWARD_SIGN.swap(sign, Ordering::Relaxed);
    log_line(format_args!(
        "spawn point direction: dot={dot:.2} forward_sign={sign} (was {before}) distance={length:.2}"
    ));
}

fn warp_manager_f32(manager: &mut SummonBuddyManager, offset: usize) -> *mut f32 {
    unsafe {
        manager
            .warp_manager
            .as_ptr()
            .cast::<u8>()
            .add(offset)
            .cast::<f32>()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BossZoneEvent {
    Entered,
    /// Left after this long in the zone.
    Left(Duration),
}

/// Boss zone: exactly while the game runs a boss fight, plus
/// BOSS_FIGHT_END_GRACE after its flag goes off (boss defeated, player dead
/// or gone).
fn boss_zone_step(
    zone: &mut Option<BossZone>,
    fighting: bool,
    now: Instant,
) -> Option<BossZoneEvent> {
    match zone {
        None if fighting => {
            *zone = Some(BossZone {
                since: now,
                fight_off_since: None,
            });
            Some(BossZoneEvent::Entered)
        }
        None => None,
        Some(current) if fighting => {
            current.fight_off_since = None;
            None
        }
        Some(current) => {
            let off = *current.fight_off_since.get_or_insert(now);
            if now.saturating_duration_since(off) < BOSS_FIGHT_END_GRACE {
                return None;
            }
            let stayed = now.saturating_duration_since(current.since);
            *zone = None;
            Some(BossZoneEvent::Left(stayed))
        }
    }
}

fn update_boss_zone(state: &mut AreaFollowState, boss_fight: Option<bool>, now: Instant) {
    if boss_fight != state.last_boss_fight {
        log_line(format_args!(
            "boss fight active: {:?} -> {:?}",
            state.last_boss_fight, boss_fight
        ));
        state.last_boss_fight = boss_fight;
    }
    match boss_zone_step(&mut state.boss_zone, boss_fight == Some(true), now) {
        Some(BossZoneEvent::Entered) => log_line(
            "boss zone entered (no warp to the player, no stuck or jump warps, BUDDY_NO_FOLLOW on every Ash)",
        ),
        Some(BossZoneEvent::Left(stayed)) => log_line(format_args!(
            "boss zone left: boss fight over, stayed_s={}",
            stayed.as_secs()
        )),
        None => {}
    }
}

/// Profile for the current zone, mount and free-fighting choice.
fn warp_profile_for(
    boss_zone: bool,
    riding: bool,
    (free_boss, free_open): (bool, bool),
) -> WarpProfile {
    if boss_zone && free_boss {
        WarpProfile::Boss
    } else if riding || !free_open {
        WarpProfile::Riding
    } else {
        WarpProfile::OnFoot
    }
}

/// Ray block time, distance and path-stacked time written for a profile.
fn warp_trigger_values(profile: WarpProfile, saved: [f32; 3]) -> [f32; 3] {
    match profile {
        WarpProfile::Boss => [
            BOSS_ZONE_TIME_RAY_BLOCK,
            BOSS_ZONE_DIST_TO_PLAYER,
            BOSS_ZONE_TIME_PATH_STACKED,
        ],
        WarpProfile::OnFoot => [
            saved[0].max(OPEN_WORLD_TIME_RAY_BLOCK),
            saved[1].max(OPEN_WORLD_DIST_TO_PLAYER),
            saved[2],
        ],
        WarpProfile::Riding => saved,
    }
}

/// The game's warp triggers are only replaced when they look like its own
/// values (a wrong offset must not be written).
fn sane_warp_triggers(read: [f32; 3]) -> bool {
    read.iter().all(|value| (0.5..=200.0).contains(value))
}

/// A new SummonBuddyWarpManager (save loaded, back from the title) starts
/// with the game's values and no warps: the state of the old one is dropped.
fn track_warp_manager(manager: &SummonBuddyManager, state: &mut AreaFollowState) {
    let addr = manager.warp_manager.as_ptr() as usize;
    if state.warp_manager_addr == Some(addr) {
        return;
    }
    if state.warp_manager_addr.is_some() {
        log_line(format_args!(
            "warp manager changed: 0x{addr:x} (profile {:?} and boss zone reset)",
            state.warp_profile
        ));
    }
    state.warp_manager_addr = Some(addr);
    state.warp_profile = None;
    state.saved_triggers = None;
    state.boss_zone = None;
    state.fog_trace = None;
    state.fog_since = None;
    state.fog_crossing = false;
    state.pending_since = None;
    state.raised_at = None;
    state.check_at = None;
    state.raised_first_check = None;
    state.stuck.clear();
    state.fight_until = None;
}

/// Whether switching area follow off has anything to undo.
pub(crate) fn area_follow_needs_release() -> bool {
    AREA_FOLLOW.lock().is_ok_and(|state| {
        state.warp_profile.is_some() || state.no_follow_active > 0 || state.boss_zone.is_some()
    })
}

/// Area follow (or the mod) switched off: the game's warp triggers go back,
/// BUDDY_NO_FOLLOW comes off every Ash and the state starts over.
pub(crate) fn release_area_follow(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
) {
    let Ok(mut state) = AREA_FOLLOW.lock() else {
        return;
    };
    track_warp_manager(manager, &mut state);
    let restored = match (state.warp_profile, state.saved_triggers) {
        (Some(profile), Some(saved)) if profile != WarpProfile::Riding => {
            for (offset, value) in WARP_TRIGGER_OFFSETS.into_iter().zip(saved) {
                unsafe { warp_manager_f32(manager, offset).write_volatile(value) };
            }
            true
        }
        _ => false,
    };
    if state.raised_at.is_some() {
        let flag = unsafe {
            manager
                .warp_manager
                .as_ptr()
                .cast::<u8>()
                .add(WARP_MANAGER_WARP_ALL_OFFSET)
        };
        if unsafe { flag.read_volatile() } == 1 {
            unsafe { flag.write_volatile(0) };
        }
    }
    let mut removed = 0u32;
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            if group.is_remote || !buddy_to_trigger.contains_key(&group.buddy_param_id) {
                continue;
            }
            if !chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }
            let chr = unsafe { group.chr_ins.as_mut() };
            let has = chr
                .special_effect
                .as_ref()
                .entries()
                .any(|entry| entry.param_id == BUDDY_NO_FOLLOW_SP_EFFECT);
            if has && let Some(enemy) = chr.as_subclass_mut::<EnemyIns>() {
                enemy.remove_speffect(BUDDY_NO_FOLLOW_SP_EFFECT);
                removed += 1;
            }
        }
    }
    log_line(format_args!(
        "area follow released: warp_triggers_restored={restored} no_follow_removed={removed}"
    ));
    let addr = state.warp_manager_addr;
    *state = AreaFollowState::default();
    state.warp_manager_addr = addr;
}

/// Writes the native warp triggers of the current profile when it changes.
fn update_warp_profile(manager: &mut SummonBuddyManager, state: &mut AreaFollowState) {
    let saved = match state.saved_triggers {
        Some(saved) => saved,
        None => {
            let read = WARP_TRIGGER_OFFSETS
                .map(|offset| unsafe { warp_manager_f32(manager, offset).read_volatile() });
            // Only sane game values are replaced (a wrong offset must not be
            // written); checked once, before any write.
            if !sane_warp_triggers(read) {
                if state.warp_profile.is_none() {
                    log_line(format_args!(
                        "warp triggers left alone: unexpected values ray_block={:.2} dist={:.2} path_stacked={:.2}",
                        read[0], read[1], read[2]
                    ));
                    state.warp_profile = Some(WarpProfile::Riding);
                }
                return;
            }
            state.saved_triggers = Some(read);
            read
        }
    };
    let profile = warp_profile_for(state.boss_zone.is_some(), state.riding, free_fighting());
    if state.warp_profile == Some(profile) {
        return;
    }
    let values = warp_trigger_values(profile, saved);
    for (offset, value) in WARP_TRIGGER_OFFSETS.into_iter().zip(values) {
        unsafe { warp_manager_f32(manager, offset).write_volatile(value) };
    }
    state.warp_profile = Some(profile);
    log_line(format_args!(
        "warp triggers: profile={profile:?} ray_block={:.1} dist={:.1} path_stacked={:.1} (game {:.1}/{:.1}/{:.1})",
        values[0], values[1], values[2], saved[0], saved[1], saved[2]
    ));
}

/// Farthest an Ash in a fight keeps (or gets) BUDDY_NO_FOLLOW off a boss zone.
fn no_follow_distance_limit(has_marker: bool) -> f32 {
    if has_marker {
        OPEN_WORLD_DIST_TO_PLAYER + NO_FOLLOW_HYSTERESIS
    } else {
        OPEN_WORLD_DIST_TO_PLAYER
    }
}

/// BUDDY_NO_FOLLOW: on every Ash in a boss zone; elsewhere, on foot, on an
/// Ash within 100 m while any Ash fought in the last 10 s, so it
/// fights where its target is instead of walking back at 23 m. Off while
/// riding and once the fight is over, so the Ashes follow again.
fn update_no_follow(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    dismissed_effects: &HashSet<i32>,
    state: &mut AreaFollowState,
    player_position: Option<HavokPosition>,
    now: Instant,
) {
    let (free_boss, free_open) = free_fighting();
    let boss = state.boss_zone.is_some() && free_boss;
    let mut applied = 0u32;
    let mut removed = 0u32;
    let mut active = 0usize;
    let mut present = HashSet::new();
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                continue;
            };
            if group.is_remote || !chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }
            let usable = !group.disappear_requested && !dismissed_effects.contains(&effect);
            let chr = unsafe { group.chr_ins.as_mut() };
            let key = std::ptr::from_ref(&*chr) as usize;
            present.insert(key);
            let position = chr.modules.as_ref().physics.as_ref().position;
            let distance = player_position.map_or(f32::MAX, |player| {
                distance_3d(
                    [position.0, position.1, position.2],
                    [player.0, player.1, player.2],
                )
            });
            let has = chr
                .special_effect
                .as_ref()
                .entries()
                .any(|entry| entry.param_id == BUDDY_NO_FOLLOW_SP_EFFECT);
            let fighting = if boss || state.riding || !usable || !free_open {
                false
            } else {
                // Attacks, dodges, guards and hits (3000-9999, as Melina's
                // keep_calm) of any Ash keep the whole group's fight going:
                // an Ash between two targets, or one that just spotted an
                // enemy, stays with the fight instead of walking back.
                let animation = current_animation(chr).0 % 1_000_000;
                if (3000..10000).contains(&animation) {
                    if state.fight_until.is_none_or(|until| now >= until) {
                        log_line(format_args!(
                            "ash fight started: effect={effect} animation={animation} distance={distance:.1}"
                        ));
                    }
                    state.fight_until = Some(now + BATTLE_GRACE);
                }
                let fighting = state.fight_until.is_some_and(|until| now < until)
                    && distance <= no_follow_distance_limit(has);
                if state.battle_logged.get(&key) != Some(&fighting) {
                    state.battle_logged.insert(key, fighting);
                    log_line(format_args!(
                        "ash fighting: chr=0x{key:x} effect={effect} fighting={fighting} animation={animation} distance={distance:.1}"
                    ));
                }
                fighting
            };
            let wanted = usable && (boss || fighting);
            if wanted {
                active += 1;
            }
            if wanted == has {
                continue;
            }
            if let Some(enemy) = chr.as_subclass_mut::<EnemyIns>() {
                if wanted {
                    enemy.apply_speffect(BUDDY_NO_FOLLOW_SP_EFFECT, true);
                    applied += 1;
                } else {
                    enemy.remove_speffect(BUDDY_NO_FOLLOW_SP_EFFECT);
                    removed += 1;
                }
            }
        }
    }
    state.battle_logged.retain(|key, _| present.contains(key));
    state.no_follow_active = active;
    let context = if boss { "boss_zone" } else { "battle" };
    if applied > 0 {
        let first = state.no_follow_applied == 0;
        state.no_follow_applied += applied;
        // First time, then every 100 to show a marker that does not stay.
        if first || state.no_follow_applied % 100 < applied {
            log_line(format_args!(
                "no-follow: context={context} applied={applied} active={active} total_applied={}",
                state.no_follow_applied
            ));
        }
    }
    if removed > 0 && active == 0 {
        log_line(format_args!(
            "no-follow removed: ashes={removed} total_applied={}",
            state.no_follow_applied
        ));
        state.no_follow_applied = 0;
    }
}

/// For a few seconds after a fog-gate warp: logs every stage change and,
/// when an entry gets its destination (stage 2, fade-out running), moves it
/// to a row in front of the player if the game's spot is on the same level.
fn trace_fog_warp(
    manager: &mut SummonBuddyManager,
    state: &mut AreaFollowState,
    player_position: Option<HavokPosition>,
    player_orientation: Option<[f32; 4]>,
    now: Instant,
) {
    let Some(trace) = &mut state.fog_trace else {
        return;
    };
    let elapsed = now.saturating_duration_since(trace.start);
    if elapsed > FOG_TRACE_WINDOW {
        log_line(format_args!(
            "fog warp trace end: t_ms={} stages=[{}]",
            elapsed.as_millis(),
            native_warp_entry_stages(manager)
        ));
        state.fog_trace = None;
        return;
    }
    let stages = native_warp_entry_stages(manager);
    if stages != trace.stages {
        log_line(format_args!(
            "fog warp trace: t_ms={} stages=[{}] -> [{stages}]",
            elapsed.as_millis(),
            trace.stages
        ));
        trace.stages = stages;
    }
    let (Some(player), Some(forward)) = (
        player_position,
        player_orientation.and_then(horizontal_forward),
    ) else {
        return;
    };
    let sign = f32::from(FORWARD_SIGN.load(Ordering::Relaxed));
    let forward = [forward[0] * sign, forward[1] * sign];
    let player = [player.0, player.1, player.2];
    let count = manager.warp_manager.as_ref().entries.iter().count();
    for (index, pair) in manager.warp_manager.as_ref().entries.iter().enumerate() {
        let base = std::ptr::from_ref(&pair.second).cast::<u8>().cast_mut();
        let key = base as usize;
        if unsafe { base.read_volatile() } != 2 || trace.placed.contains(&key) {
            continue;
        }
        trace.placed.insert(key);
        // Real layout: stage at +0x00, destination at +0x10, rotation at
        // +0x20 (BN of 0x1404c3180: node+0x30 / +0x40 / +0x50).
        let destination = unsafe { base.add(0x10).cast::<[f32; 4]>() };
        let game = unsafe { destination.read_unaligned() };
        let game_spot = [game[0], game[1], game[2]];
        let spot = front_spot(player, forward, index, count);
        let near = distance_3d(game_spot, player) <= FOG_FRONT_MAX_GAME_DISTANCE;
        let level = (game[1] - player[1]).abs() <= FOG_FRONT_MAX_HEIGHT;
        let applied = near && level;
        if applied {
            unsafe { destination.write_unaligned([spot[0], game[1], spot[2], game[3]]) };
        }
        log_line(format_args!(
            "fog warp destination: t_ms={} entry={index} applied={applied} game=({:.2},{:.2},{:.2}) front=({:.2},{:.2},{:.2}) player=({:.2},{:.2},{:.2}) near={near} level={level} forward_sign={sign}",
            elapsed.as_millis(),
            game[0],
            game[1],
            game[2],
            spot[0],
            game[1],
            spot[2],
            player[0],
            player[1],
            player[2],
        ));
    }
}

/// The end of the player's fog-gate walk opens a warp all for every live Ash.
fn detect_fog_gate(state: &mut AreaFollowState, animation: i32, now: Instant) {
    let event = animation.rem_euclid(1_000_000);
    if (60000..61000).contains(&event) && event != state.last_event_animation {
        log_line(format_args!(
            "player event animation: {animation} fog_gate={}",
            is_fog_gate_animation(animation)
        ));
    }
    if (60000..61000).contains(&event) {
        state.last_event_animation = event;
    }
    if let Some(played) = fog_gate_step(&mut state.fog_since, animation, now) {
        log_line(format_args!(
            "fog gate crossed: played_ms={} next_animation={animation}",
            played.as_millis()
        ));
        state.pending_since = Some(now);
        state.fog_crossing = true;
    }
}

/// Stage byte of every native warp entry, in entry order. Rust places the
/// 16-aligned entry at node+0x30, which is where 0x1404c3180 reads the stage
/// (`i[6].b`), so the stage is raw byte 0 of `pair.second`, not the SDK's +0x08.
fn native_warp_entry_stages(manager: &SummonBuddyManager) -> String {
    manager
        .warp_manager
        .as_ref()
        .entries
        .iter()
        .map(|pair| unsafe { *std::ptr::from_ref(&pair.second).cast::<u8>() }.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn native_warp_any_entry_busy(manager: &SummonBuddyManager) -> bool {
    manager
        .warp_manager
        .as_ref()
        .entries
        .iter()
        .any(|pair| unsafe { *std::ptr::from_ref(&pair.second).cast::<u8>() } != 0)
}

/// Live, non-dismissed Ashes as (count, max distance to the player).
fn live_ash_distances(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    dismissed_effects: &HashSet<i32>,
    player: HavokPosition,
) -> (usize, f32) {
    let mut count = 0usize;
    let mut max_distance = 0.0f32;
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                continue;
            };
            if group.is_remote || group.disappear_requested || dismissed_effects.contains(&effect) {
                continue;
            }
            let chr = unsafe { group.chr_ins.as_ref() };
            if !chr_alive(chr) {
                continue;
            }
            let position = chr.modules.as_ref().physics.as_ref().position;
            let distance = distance_3d(
                [position.0, position.1, position.2],
                [player.0, player.1, player.2],
            );
            count += 1;
            max_distance = max_distance.max(distance);
        }
    }
    (count, max_distance)
}

/// After a player jump raises the native "warp all" flag once, so the game
/// warps every registered Ash with its own fade. The flag is lowered on the
/// next pass. An Ash still beyond NATIVE_WARP_ALL_FAIL_DISTANCE after 4 s,
/// with no warp in progress, turns the feature off until the next start,
/// unless the player rides or the Ashes are free to fight (boss zone,
/// BUDDY_NO_FOLLOW).
fn update_native_warp_all(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    dismissed_effects: &HashSet<i32>,
    state: &mut AreaFollowState,
    player_position: Option<HavokPosition>,
    now: Instant,
) {
    let flag = unsafe {
        manager
            .warp_manager
            .as_ptr()
            .cast::<u8>()
            .add(WARP_MANAGER_WARP_ALL_OFFSET)
    };

    if let Some(raised_at) = state.raised_at
        && now > raised_at
    {
        let value = unsafe { flag.read_volatile() };
        unsafe { flag.write_volatile(0) };
        state.raised_at = None;
        log_line(format_args!(
            "native warp all lowered: n={} flag_before={} elapsed_ms={} stages=[{}]",
            state.count,
            value,
            now.saturating_duration_since(raised_at).as_millis(),
            native_warp_entry_stages(manager),
        ));
    }

    let Some(player) = player_position else {
        return;
    };

    if let Some(check_at) = state.check_at
        && now >= check_at
    {
        state.check_at = None;
        let (count, max_distance) =
            live_ash_distances(manager, buddy_to_trigger, dismissed_effects, player);
        let far = count > 0 && max_distance > NATIVE_WARP_ALL_FAIL_DISTANCE;
        // 0.1.162: an entry still in a warp stage is a slow warp, not a
        // failed one: check again later instead of disabling.
        let busy = native_warp_any_entry_busy(manager);
        let waiting = far
            && busy
            && state.raised_first_check.is_some_and(|first| {
                now.saturating_duration_since(first) < NATIVE_WARP_ALL_BUSY_WAIT_MAX
            });
        // 1.0.2: on Torrent the player outruns any warp (42 m after 4 s in
        // the test), which is not a broken warp: no switch-off while riding.
        let outrun = far && !waiting && state.riding;
        // 1.0.2: in a boss zone or a fight the Ashes are meant to be far.
        let free =
            far && !waiting && !outrun && (state.boss_zone.is_some() || state.no_follow_active > 0);
        let failed = far && !waiting && !outrun && !free;
        if waiting {
            state.check_at = Some(now + NATIVE_WARP_ALL_BUSY_RECHECK);
        } else {
            state.raised_first_check = None;
        }
        if failed {
            state.disabled = true;
        }
        log_line(format_args!(
            "native warp all check: n={} outcome={} ashes={} max_distance={:.1} stages=[{}] disabled={}",
            state.count,
            if failed {
                "failed"
            } else if outrun {
                "outrun_riding"
            } else if free {
                "free_to_fight"
            } else if waiting {
                "warp_in_progress"
            } else {
                "held"
            },
            count,
            max_distance,
            native_warp_entry_stages(manager),
            state.disabled,
        ));
    }

    let Some(pending_since) = state.pending_since else {
        return;
    };
    let waited = now.saturating_duration_since(pending_since);
    if waited > NATIVE_WARP_ALL_PENDING_WINDOW {
        state.pending_since = None;
        state.fog_crossing = false;
        return;
    }
    // The fog walk is over when the crossing is seen: no settle time.
    let settle = if state.fog_crossing {
        Duration::ZERO
    } else {
        NATIVE_WARP_ALL_SETTLE
    };
    if waited < settle
        || state.disabled
        || state.raised_at.is_some()
        || state
            .last_at
            .is_some_and(|last| now.saturating_duration_since(last) < NATIVE_WARP_ALL_INTERVAL)
    {
        return;
    }
    let (count, max_distance) =
        live_ash_distances(manager, buddy_to_trigger, dismissed_effects, player);
    // Behind a fog wall an Ash can be only a few metres away.
    let min_distance = if state.fog_crossing {
        0.0
    } else {
        NATIVE_WARP_ALL_MIN_DISTANCE
    };
    if count == 0 || max_distance <= min_distance {
        return;
    }
    let entries = manager.warp_manager.as_ref().entries.iter().count();
    if entries == 0 {
        return;
    }
    if state.fog_crossing {
        raise_fog_gate_entries(manager, state, now, waited, count, entries, max_distance);
        return;
    }
    let value = unsafe { flag.read_volatile() };
    if value > 1 {
        state.disabled = true;
        log_line(format_args!(
            "native warp all disabled: unexpected flag value {} at warp_manager+0x{:x}",
            value, WARP_MANAGER_WARP_ALL_OFFSET
        ));
        return;
    }
    unsafe { flag.write_volatile(1) };
    state.count += 1;
    state.pending_since = None;
    state.raised_at = Some(now);
    state.last_at = Some(now);
    state.check_at = Some(now + NATIVE_WARP_ALL_CHECK_DELAY);
    state.raised_first_check = Some(now);
    log_line(format_args!(
        "native warp all raised: n={} cause=jump after_jump_ms={} ashes={} entries={} max_distance={:.1} flag_before={} stages=[{}]",
        state.count,
        waited.as_millis(),
        count,
        entries,
        max_distance,
        value,
        native_warp_entry_stages(manager),
    ));
}

/// Fog gate: every idle native warp entry goes to stage 1, as the stuck path
/// does for one Ash. The 1.0.2 test showed the "warp all" flag left unread
/// there (still 1 a frame later, every stage 0), while per-entry stages run
/// the game's warp with its fade.
fn raise_fog_gate_entries(
    manager: &mut SummonBuddyManager,
    state: &mut AreaFollowState,
    now: Instant,
    waited: Duration,
    ashes: usize,
    entries: usize,
    max_distance: f32,
) {
    let stages_before = native_warp_entry_stages(manager);
    let mut raised = 0usize;
    for pair in manager.warp_manager.as_ref().entries.iter() {
        let ptr = std::ptr::from_ref(&pair.second).cast::<u8>().cast_mut();
        if unsafe { ptr.read_volatile() } == 0 {
            unsafe { ptr.write_volatile(1) };
            raised += 1;
        }
    }
    state.fog_crossing = false;
    state.fog_trace = Some(FogTrace {
        start: now,
        stages: native_warp_entry_stages(manager),
        placed: HashSet::new(),
    });
    state.count += 1;
    state.pending_since = None;
    state.last_at = Some(now);
    // No 4 s check: the boss zone starts now and the Ashes may run off after
    // the boss at once (a far Ash there is not a failed warp).
    log_line(format_args!(
        "native warp entries raised: n={} cause=fog_gate after_crossing_ms={} ashes={ashes} entries={entries} raised={raised} max_distance={max_distance:.1} stages_before=[{stages_before}]",
        state.count,
        waited.as_millis(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fog_gate_crossing_needs_the_whole_walk() {
        let start = Instant::now();
        let mut since = None;
        assert_eq!(fog_gate_step(&mut since, 0, start), None);
        assert_eq!(fog_gate_step(&mut since, 60060, start), None);
        assert_eq!(
            fog_gate_step(&mut since, 60060, start + Duration::from_millis(1500)),
            None
        );
        let played = fog_gate_step(&mut since, 0, start + Duration::from_millis(2500));
        assert_eq!(played, Some(Duration::from_millis(2500)));
        assert_eq!(
            fog_gate_step(&mut since, 0, start + Duration::from_millis(2600)),
            None
        );
        // An interrupted walk is not a crossing.
        assert_eq!(
            fog_gate_step(&mut since, 60060, start + Duration::from_secs(5)),
            None
        );
        assert_eq!(
            fog_gate_step(&mut since, 0, start + Duration::from_millis(5100)),
            None
        );
        // Ids with a TAE prefix (a00 = 000xxxxxx) still match.
        assert!(is_fog_gate_animation(60060));
        assert!(!is_fog_gate_animation(60070));
        assert!(is_fog_gate_animation(1_060_060));
    }

    #[test]
    fn warp_trigger_profiles() {
        let game = [5.0, 33.0, 3.0];
        assert!(sane_warp_triggers(game));
        assert!(!sane_warp_triggers([0.0, 33.0, 3.0]));
        assert!(!sane_warp_triggers([5.0, 1e9, 3.0]));
        assert!(!sane_warp_triggers([5.0, f32::NAN, 3.0]));
        assert_eq!(warp_trigger_values(WarpProfile::Riding, game), game);
        assert_eq!(
            warp_trigger_values(WarpProfile::OnFoot, game),
            [OPEN_WORLD_TIME_RAY_BLOCK, OPEN_WORLD_DIST_TO_PLAYER, 3.0]
        );
        // A game value already above the on-foot floor is kept.
        assert_eq!(
            warp_trigger_values(WarpProfile::OnFoot, [40.0, 150.0, 3.0]),
            [40.0, 150.0, 3.0]
        );
        let boss = warp_trigger_values(WarpProfile::Boss, game);
        assert!(boss.iter().all(|value| *value >= 100_000.0));
    }

    #[test]
    fn boss_zone_follows_the_boss_fight() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut zone = None;
        // A fog wall with no boss fight: no zone.
        assert_eq!(boss_zone_step(&mut zone, false, at(0)), None);
        assert!(zone.is_none());
        assert_eq!(
            boss_zone_step(&mut zone, true, at(100)),
            Some(BossZoneEvent::Entered)
        );
        // A short drop of the flag (phase change) keeps the zone.
        assert_eq!(boss_zone_step(&mut zone, false, at(1000)), None);
        assert_eq!(boss_zone_step(&mut zone, false, at(3500)), None);
        assert_eq!(boss_zone_step(&mut zone, true, at(3600)), None);
        assert_eq!(boss_zone_step(&mut zone, false, at(4000)), None);
        assert!(zone.is_some());
        // Over for the whole grace: the zone ends.
        assert_eq!(
            boss_zone_step(&mut zone, false, at(7000)),
            Some(BossZoneEvent::Left(Duration::from_millis(6900)))
        );
        assert!(zone.is_none());
    }

    #[test]
    fn free_fighting_choice_picks_the_profile() {
        let everywhere = free_fighting_scopes(FREE_FIGHTING_EVERYWHERE);
        let bosses = free_fighting_scopes(FREE_FIGHTING_BOSSES);
        let off = free_fighting_scopes(FREE_FIGHTING_OFF);
        assert_eq!(everywhere, (true, true));
        assert_eq!(bosses, (true, false));
        assert_eq!(off, (false, false));
        assert_eq!(warp_profile_for(true, false, everywhere), WarpProfile::Boss);
        assert_eq!(warp_profile_for(true, true, everywhere), WarpProfile::Boss);
        assert_eq!(
            warp_profile_for(false, false, everywhere),
            WarpProfile::OnFoot
        );
        assert_eq!(
            warp_profile_for(false, true, everywhere),
            WarpProfile::Riding
        );
        assert_eq!(warp_profile_for(true, false, bosses), WarpProfile::Boss);
        assert_eq!(warp_profile_for(false, false, bosses), WarpProfile::Riding);
        assert_eq!(warp_profile_for(true, false, off), WarpProfile::Riding);
        assert_eq!(warp_profile_for(false, false, off), WarpProfile::Riding);
    }

    #[test]
    fn no_follow_edge_has_hysteresis() {
        assert_eq!(no_follow_distance_limit(false), OPEN_WORLD_DIST_TO_PLAYER);
        assert!(no_follow_distance_limit(true) > OPEN_WORLD_DIST_TO_PLAYER);
    }

    #[test]
    fn front_row_is_ahead_and_side_by_side() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
        let ahead = horizontal_forward([0.0, 0.0, 0.0, 1.0]).unwrap();
        assert!(close(ahead[0], 0.0) && close(ahead[1], 1.0));
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let turned = horizontal_forward([0.0, half, 0.0, half]).unwrap();
        assert!(close(turned[0], 1.0) && close(turned[1], 0.0));
        let one = front_spot([1.0, 2.0, 3.0], ahead, 0, 1);
        assert!(close(one[0], 1.0) && close(one[1], 2.0) && close(one[2], 6.0));
        let left = front_spot([0.0, 0.0, 0.0], ahead, 0, 3);
        let middle = front_spot([0.0, 0.0, 0.0], ahead, 1, 3);
        let right = front_spot([0.0, 0.0, 0.0], ahead, 2, 3);
        assert!(close(middle[0], 0.0) && close(middle[2], FOG_FRONT_DISTANCE));
        assert!(close(left[0], -FOG_FRONT_SPACING) && close(right[0], FOG_FRONT_SPACING));
    }
}
