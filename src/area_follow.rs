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
//! Stuck Ashes (0.1.161, simplified): an Ash farther than 25 m that does not
//! get 1.5 m closer for 4 s (or is beyond 75 m) while the player has not just
//! jumped has its own native warp entry set to stage 1, the state "warp all"
//! sets, so the game warps only that Ash with its fade.

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
const STUCK_FAR_DISTANCE: f32 = 75.0;
const STUCK_PROGRESS_STEP: f32 = 1.5;
const STUCK_WAIT: Duration = Duration::from_millis(4000);
const STUCK_COOLDOWN: Duration = Duration::from_millis(5000);
const STUCK_PLAYER_TRAVEL: f32 = 8.0;

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
) {
    let Ok(mut state) = AREA_FOLLOW.lock() else {
        return;
    };
    let now = Instant::now();
    detect_player_position_discontinuity(&mut state, player_position, now);
    update_native_warp_all(
        manager,
        buddy_to_trigger,
        dismissed_effects,
        &mut state,
        player_position,
        now,
    );
    // A crossing is the warp-all's job; per-Ash warps wait until it is over.
    let crossing = state.pending_since.is_some() || state.raised_at.is_some() || state.check_at.is_some();
    if crossing {
        state.stuck.clear();
    } else if let Some(player) = player_position {
        update_stuck_ashes(manager, buddy_to_trigger, dismissed_effects, &mut state, player, now);
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
            if distance < STUCK_DISTANCE {
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
                if distance < STUCK_FAR_DISTANCE {
                    trace.since = now;
                    trace.player_anchor = player_here;
                }
                continue;
            }
            let player_travelled =
                distance_3d(player_here, trace.player_anchor) >= STUCK_PLAYER_TRAVEL;
            if !player_travelled && distance < STUCK_FAR_DISTANCE {
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
) {
    let Some(position) = player_position else {
        return;
    };
    let current = [position.0, position.1, position.2];
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
        }
    }
    state.last_player_sample = Some((current, now));
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
/// with no warp in progress, turns the feature off until the next start.
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
        let failed = far && !waiting;
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
        return;
    }
    if waited < NATIVE_WARP_ALL_SETTLE
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
    if count == 0 || max_distance <= NATIVE_WARP_ALL_MIN_DISTANCE {
        return;
    }
    let entries = manager.warp_manager.as_ref().entries.iter().count();
    if entries == 0 {
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
        "native warp all raised: n={} after_jump_ms={} ashes={} entries={} max_distance={:.1} flag_before={} stages=[{}]",
        state.count,
        waited.as_millis(),
        count,
        entries,
        max_distance,
        value,
        native_warp_entry_stages(manager),
    ));
}
