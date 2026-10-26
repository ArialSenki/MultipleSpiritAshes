//! NativeTweaks extras for the built-in Multiple Spirit Ashes: dismiss every
//! Ash at once, a cost for additional Ashes and less damage per extra Ash.

use super::*;

static DISMISS_ALL_EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
/// When the request was made (ms since DISMISS_ALL_EPOCH, +1); 0 = none.
static DISMISS_ALL_REQUESTED_AT: AtomicU64 = AtomicU64::new(0);
/// A request the policy pass could not take soon (feature off, loading
/// screen) is dropped, so it never dismisses the Ashes much later.
const DISMISS_ALL_MAX_AGE_MS: u64 = 2000;

fn dismiss_all_clock_ms() -> u64 {
    DISMISS_ALL_EPOCH.elapsed().as_millis().min(u64::MAX as u128 - 1) as u64 + 1
}

/// Asks the next policy pass to dismiss every living Ash (Hub button/hotkey).
#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_dismiss_all() {
    DISMISS_ALL_REQUESTED_AT.store(dismiss_all_clock_ms(), Ordering::Release);
}

/// Seconds a fallen Ash's group is kept before it is released.
const DEAD_GROUP_RELEASE_DELAY_SECONDS: f32 = 2.0;

/// Releases the groups of fallen Ashes. The native owner-wide clear used to
/// drop them on the next summon; with that clear bypassed to keep the living
/// Ashes, dead groups piled up (user log: 10 groups, 8 at HP 0) and the game
/// then refused new summons although the summoning animation played. Only
/// local groups whose character is dead and not already leaving are marked;
/// every guard that undoes removal marks skips dead characters.
pub(crate) fn release_dead_groups(manager: &mut SummonBuddyManager) {
    let mut released = Vec::new();
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            if group.is_remote || group.disappear_requested || chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }
            group.disappear_requested = true;
            group.disappear_delay_sec = DEAD_GROUP_RELEASE_DELAY_SECONDS;
            released.push(group.buddy_param_id);
        }
    }
    if !released.is_empty() {
        log_line(format_args!("dead groups released: buddies={released:?}"));
    }
}

/// Feature switched off: a pending request is dropped.
pub(crate) fn clear_dismiss_all_request() {
    DISMISS_ALL_REQUESTED_AT.store(0, Ordering::Release);
}

/// Runs in the post-physics policy task: every living Ash gets the same
/// fade-out as a dismissal with its item (update_dismissal_fades fades it).
pub(crate) fn take_dismiss_all_request(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    state: &mut RuntimeState,
) {
    let requested_at = DISMISS_ALL_REQUESTED_AT.swap(0, Ordering::AcqRel);
    if requested_at == 0
        || dismiss_all_clock_ms().saturating_sub(requested_at) > DISMISS_ALL_MAX_AGE_MS
    {
        return;
    }
    let mut dismissed = HashSet::new();
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                continue;
            };
            if group.is_remote
                || group.disappear_requested
                || !chr_alive(unsafe { group.chr_ins.as_ref() })
            {
                continue;
            }
            group.disappear_delay_sec = DISMISS_FADE_SECONDS;
            group.disappear_requested = true;
            dismissed.insert(effect);
        }
    }
    state.dismissed_effects.extend(dismissed.iter().copied());
    log_line(format_args!(
        "dismiss all: effects={:?}",
        dismissed.iter().collect::<Vec<_>>()
    ));
}

/// Goods of the pending new Ash whose consumeMP was scaled: (original, written).
static EXTRA_COST_OVERRIDES: LazyLock<Mutex<HashMap<i32, (i16, i16)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Cost of additional Ashes (%): while a new Ash is pending (the DoSummon
/// latch) and another is already alive, its item's consumeMP is scaled, so the
/// FP check and the FP spent both use the scaled cost. Back to its value once
/// the Ash is alive or the request is gone. The value is taken when scaled and
/// written back only if still the scaled one, so the Hub's Spirit ash cost (%)
/// slider is never undone. Runs before the low-FP dismissal pass.
pub(crate) fn update_extra_ash_cost(live_effects: &HashSet<i32>, active_goods_ids: &HashSet<i32>) {
    let Ok(mut overrides) = EXTRA_COST_OVERRIDES.lock() else {
        return;
    };
    let percent = settings().extra_cost_percent;
    let pending_goods = PENDING_SUMMON_GATE_LATCH_GOODS_ID.load(Ordering::Relaxed) as i32;
    let desired = (percent != 100
        && ALLOW_PENDING_SUMMON_DO_SUMMON.load(Ordering::Acquire)
        && !live_effects.is_empty()
        && pending_goods > 0
        && !live_effects.contains(&pending_goods)
        && !active_goods_ids.contains(&pending_goods))
    .then_some(pending_goods);
    if overrides.is_empty() && desired.is_none() {
        return;
    }
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    overrides.retain(|goods_id, (original, written)| {
        if desired == Some(*goods_id) {
            return true;
        }
        if let Some(row) = repository.get_mut::<EquipParamGoods>(*goods_id as u32)
            && row.consume_mp() == *written
        {
            row.set_consume_mp(*original);
        }
        false
    });
    let Some(goods_id) = desired else {
        return;
    };
    let Some(row) = repository.get_mut::<EquipParamGoods>(goods_id as u32) else {
        return;
    };
    let current = row.consume_mp();
    let original = match overrides.get(&goods_id) {
        Some((original, written)) if current == *written => *original,
        _ => current,
    };
    let scaled = (f32::from(original) * percent as f32 / 100.0)
        .round()
        .clamp(0.0, f32::from(i16::MAX)) as i16;
    if current != scaled {
        row.set_consume_mp(scaled);
    }
    if overrides.insert(goods_id, (original, scaled)).is_none() {
        log_line(format_args!(
            "additional Ash cost: goods={goods_id} fp={original} -> {scaled} ({percent}%)"
        ));
    }
}

/// Feature switched off: every scaled consumeMP gets its value back.
pub(crate) fn restore_extra_ash_cost() {
    let Ok(mut overrides) = EXTRA_COST_OVERRIDES.lock() else {
        return;
    };
    if overrides.is_empty() {
        return;
    }
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    for (goods_id, (original, written)) in overrides.drain() {
        if let Some(row) = repository.get_mut::<EquipParamGoods>(goods_id as u32)
            && row.consume_mp() == written
        {
            row.set_consume_mp(original);
        }
    }
}

/// SpEffectParam rows Smithbox names "Area Scaling - (Unused)": none is
/// referenced by the game. The first one present carries the damage factor.
const DAMAGE_CARRIER_CANDIDATES: [u32; 8] = [7210, 7220, 7230, 7240, 7250, 7260, 7270, 7280];
const MIN_DAMAGE_FACTOR: f32 = 0.25;

struct DamageState {
    carrier: Option<Option<i32>>,
    factor: f32,
}

static DAMAGE: Mutex<DamageState> = Mutex::new(DamageState {
    carrier: None,
    factor: 1.0,
});

/// Less damage per extra Ash (%): with n living Ashes each one deals
/// 1 - k·(n-1)% of its damage (at least 25 %), through a permanent SpEffect
/// on every living Ash whose attack-power rates hold that factor. The rest of
/// the carrier row (HP, stamina, defence and resistance rates of the original
/// area-scaling row) is set neutral the first time it is used.
pub(crate) fn update_damage_by_count(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    live_effects: &HashSet<i32>,
) {
    let Ok(mut damage) = DAMAGE.lock() else {
        return;
    };
    let reduction = settings().damage_reduction_percent as f32;
    // Only the player's own Ashes count (not a co-op partner's).
    let own_live = manager
        .groups
        .iter()
        .flat_map(|entry| entry.second.iter())
        .filter(|group| !group.is_remote && chr_alive(unsafe { group.chr_ins.as_ref() }))
        .filter_map(|group| buddy_to_trigger.get(&group.buddy_param_id).copied())
        .filter(|effect| live_effects.contains(effect))
        .collect::<HashSet<_>>();
    let extra = own_live.len().saturating_sub(1) as f32;
    let factor = if settings().enabled && reduction > 0.0 && extra > 0.0 {
        (1.0 - reduction * extra / 100.0).max(MIN_DAMAGE_FACTOR)
    } else {
        1.0
    };
    if damage.carrier.is_none() && factor >= 1.0 {
        return;
    }
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let carrier = *damage.carrier.get_or_insert_with(|| {
        let found = DAMAGE_CARRIER_CANDIDATES
            .into_iter()
            .find(|id| repository.get::<SpEffectParam>(*id).is_some());
        if let Some(id) = found
            && let Some(row) = repository.get_mut::<SpEffectParam>(id)
        {
            row.set_effect_endurance(-1.0);
            row.set_max_hp_rate(1.0);
            row.set_max_stamina_rate(1.0);
            row.set_physics_diffence_rate(1.0);
            row.set_magic_diffence_rate(1.0);
            row.set_fire_diffence_rate(1.0);
            row.set_thunder_diffence_rate(1.0);
            row.set_dark_diffence_rate(1.0);
            row.set_stamina_attack_rate(1.0);
            row.set_regist_poizon_change_rate(1.0);
            row.set_regist_disease_change_rate(1.0);
            row.set_regist_blood_change_rate(1.0);
            row.set_regist_curse_change_rate(1.0);
            row.set_regist_freeze_change_rate(1.0);
            row.set_regist_sleep_change_rate(1.0);
            row.set_regist_madness_change_rate(1.0);
        }
        log_line(format_args!("damage by Ash count: carrier SpEffect={found:?}"));
        found.map(|id| id as i32)
    });
    let Some(carrier) = carrier else {
        return;
    };
    if (factor - damage.factor).abs() > f32::EPSILON {
        if let Some(row) = repository.get_mut::<SpEffectParam>(carrier as u32) {
            row.set_physics_attack_power_rate(factor);
            row.set_magic_attack_power_rate(factor);
            row.set_fire_attack_power_rate(factor);
            row.set_thunder_attack_power_rate(factor);
            row.set_dark_attack_power_rate(factor);
        }
        log_line(format_args!(
            "damage by Ash count: ashes={} factor={factor:.2}",
            own_live.len()
        ));
        damage.factor = factor;
    }
    let want = factor < 1.0;
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            if group.is_remote
                || !buddy_to_trigger.contains_key(&group.buddy_param_id)
                || !chr_alive(unsafe { group.chr_ins.as_ref() })
            {
                continue;
            }
            let chr = unsafe { group.chr_ins.as_mut() };
            let has = chr
                .special_effect
                .as_ref()
                .entries()
                .any(|entry| entry.param_id == carrier);
            if want == has {
                continue;
            }
            if let Some(enemy) = chr.as_subclass_mut::<EnemyIns>() {
                if want {
                    enemy.apply_speffect(carrier, true);
                } else {
                    enemy.remove_speffect(carrier);
                }
            }
        }
    }
}

/// Feature switched off: the carrier's damage factor back to 1, so an Ash that
/// still carries it fights normally.
pub(crate) fn reset_damage_factor() {
    let Ok(mut damage) = DAMAGE.lock() else {
        return;
    };
    if (damage.factor - 1.0).abs() <= f32::EPSILON {
        return;
    }
    let Some(Some(carrier)) = damage.carrier else {
        return;
    };
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    if let Some(row) = repository.get_mut::<SpEffectParam>(carrier as u32) {
        row.set_physics_attack_power_rate(1.0);
        row.set_magic_attack_power_rate(1.0);
        row.set_fire_attack_power_rate(1.0);
        row.set_thunder_attack_power_rate(1.0);
        row.set_dark_attack_power_rate(1.0);
    }
    damage.factor = 1.0;
}

/// Summoning-monument rules (BuddyStoneParam) for the standalone DLL; in
/// NativeTweaks the Hub's own Spirit Ashes options do this, so it never calls
/// nt_msa_set_buddy_stone_rules and nothing here runs. Values as the Hub's:
/// anywhere = activateRange 0xFFFF and no activation region for every
/// monument (see the Hub's sync_buddy), overwriteReturnRange -1 (no dismissal for walking away); keep
/// after boss = eliminateTargetEntityId 0;
/// resummon without rest = summonedEventFlagId 0. Originals are restored when
/// a rule is turned off. Applies after the area reloads (rest at a grace).
#[derive(Clone, Copy, PartialEq, Eq)]
struct StoneRuleSet {
    anywhere: bool,
    keep_after_boss: bool,
    resummon_without_rest: bool,
}

struct StoneRules {
    requested: Option<StoneRuleSet>,
    applied: Option<StoneRuleSet>,
    /// row id -> (activateRange, overwriteReturnRange, region, eliminateTarget, summonedFlag)
    originals: HashMap<u32, (u16, i16, u32, u32, u32)>,
}

static STONE_RULES: LazyLock<Mutex<StoneRules>> = LazyLock::new(|| {
    Mutex::new(StoneRules {
        requested: None,
        applied: None,
        originals: HashMap::new(),
    })
});

#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_set_buddy_stone_rules(
    anywhere: u8,
    keep_after_boss: u8,
    resummon_without_rest: u8,
) {
    if let Ok(mut rules) = STONE_RULES.lock() {
        rules.requested = Some(StoneRuleSet {
            anywhere: anywhere != 0,
            keep_after_boss: keep_after_boss != 0,
            resummon_without_rest: resummon_without_rest != 0,
        });
    }
}

pub(crate) fn update_buddy_stone_rules() {
    let Ok(mut rules) = STONE_RULES.lock() else {
        return;
    };
    let Some(wanted) = rules.requested else {
        return;
    };
    if rules.applied == Some(wanted) {
        return;
    }
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let capture = rules.originals.is_empty();
    let mut rows = 0usize;
    for (id, row) in repository.rows_mut::<BuddyStoneParam>() {
        let id = id as u32;
        if capture {
            rules.originals.insert(
                id,
                (
                    row.activate_range(),
                    row.overwrite_return_range(),
                    row.overwrite_activate_region_entity_id(),
                    row.eliminate_target_entity_id(),
                    row.summoned_event_flag_id(),
                ),
            );
        }
        let Some(&(range, return_range, region, eliminate, flag)) = rules.originals.get(&id) else {
            continue;
        };
        row.set_activate_range(if wanted.anywhere { u16::MAX } else { range });
        row.set_overwrite_return_range(if wanted.anywhere { -1 } else { return_range });
        // Opened by distance everywhere, as in the Hub (see vanilla_settings.cpp):
        // outside a region, ashes summoned from that monument freeze half-dismissed.
        row.set_overwrite_activate_region_entity_id(if wanted.anywhere { 0 } else { region });
        row.set_eliminate_target_entity_id(if wanted.keep_after_boss { 0 } else { eliminate });
        row.set_summoned_event_flag_id(if wanted.resummon_without_rest { 0 } else { flag });
        rows += 1;
    }
    if rows == 0 {
        rules.originals.clear();
        return;
    }
    rules.applied = Some(wanted);
    SUMMON_ANYWHERE.store(wanted.anywhere, Ordering::Relaxed);
    log_line(format_args!(
        "summoning monument rules: rows={rows} summon_anywhere={} keep_after_boss={} resummon_without_rest={}",
        wanted.anywhere, wanted.keep_after_boss, wanted.resummon_without_rest
    ));
}

/// Summon anywhere, beyond the monument's own map tile. Each frame the game
/// looks for a summoning monument only among the loaded ones near the player
/// and writes it to buddy_stone_entity_id (SummonBuddyManager +0x38), 0 when
/// there is none; item use is refused at 0 (r140 test log: Caelid and
/// Radahn's battlefield, buddy_stone=0 with every monument rule applied). With
/// the option on, the monument nearest to the player's map tile (else the last
/// one the game found) is put in while none is found, so the spirit ash can be used; the summon itself appears next to the
/// player (spawn origin synchronized). In NativeTweaks the Hub turns it on with
/// its Summon anywhere option (nt_msa_set_summon_anywhere); the standalone DLL
/// with its SummonAnywhere rule.
static SUMMON_ANYWHERE: AtomicBool = AtomicBool::new(false);
static LAST_STONE: AtomicU32 = AtomicU32::new(0);
static STONE_FALLBACK_LOGGED: AtomicU32 = AtomicU32::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_set_summon_anywhere(enabled: u8) {
    SUMMON_ANYWHERE.store(enabled != 0, Ordering::Relaxed);
}

/// Every monument (BuddyStoneParam row id = its entity id), read once.
static STONE_IDS: LazyLock<Mutex<Vec<u32>>> = LazyLock::new(|| Mutex::new(Vec::new()));
/// Player block (raw) -> monument picked for it, so the search runs once per tile.
static NEAREST_STONE: Mutex<(i32, u32)> = Mutex::new((i32::MIN, 0));

/// Map tile of a monument from its entity id: overworld ids are
/// 10XXYY0NNN (m60_XX_YY) or 20XXYY0NNN (m61, the DLC); legacy dungeons are
/// AABBNNNN (mAA_BB).
fn stone_tile(id: u32) -> (u8, u8, u8) {
    if id >= 1_000_000_000 {
        let area = if id / 100_000_000 == 20 { 61 } else { 60 };
        (area, ((id / 1_000_000) % 100) as u8, ((id / 10_000) % 100) as u8)
    } else {
        ((id / 1_000_000) as u8, ((id / 10_000) % 100) as u8, 0)
    }
}

/// The monument closest to the player's map tile.
fn nearest_stone() -> Option<u32> {
    let world = unsafe { WorldChrMan::instance().ok()? };
    let player = world.main_player.as_ref()?;
    let block = player.current_block_id;
    let position = &player.block_position;
    let overworld = block.area() == 60 || block.area() == 61;
    // Overworld blocks also come as larger tiles (r140 tests at Radahn:
    // m60_25_18_01 alternating with m60_51_37_00, and m60_11_09_02 before any
    // small one): index 1 covers 2x2 small tiles of 256 m, index 2 4x4. The
    // small tile comes from the position, measured from the block's centre.
    let (x, y) = if overworld {
        let scale = 1i32 << block.index().min(2);
        let half = scale as f32 * 128.0;
        let sub = |v: f32| (((v + half) / 256.0).floor() as i32).clamp(0, scale - 1);
        (block.block() as i32 * scale + sub(position.x), block.region() as i32 * scale + sub(position.z))
    } else {
        (block.block() as i32, 0)
    };
    let raw = (block.area() as i32) << 24 | x << 12 | y;
    if let Ok(cache) = NEAREST_STONE.lock() {
        if cache.0 == raw {
            return (cache.1 != 0).then_some(cache.1);
        }
    }
    let mut ids = STONE_IDS.lock().ok()?;
    if ids.is_empty() {
        let repository = unsafe { SoloParamRepository::instance().ok()? };
        ids.extend(
            repository
                .rows::<BuddyStoneParam>()
                .map(|(id, _)| id as u32),
        );
    }
    let best = ids
        .iter()
        .filter_map(|&id| {
            let (area, sx, sy) = stone_tile(id);
            (area == block.area()).then(|| ((sx as i32 - x).abs().max((sy as i32 - y).abs()), id))
        })
        .min()
        .map(|(_, id)| id)
        .unwrap_or(0);
    if let Ok(mut cache) = NEAREST_STONE.lock() {
        *cache = (raw, best);
    }
    log_line(format_args!(
        "summon anywhere: player in m{:02}_{:02}_{:02}_{:02} at ({:.0}, {:.0}) = tile {x}_{y}; nearest monument {best}",
        block.area(),
        block.block(),
        block.region(),
        block.index(),
        position.x,
        position.z
    ));
    (best != 0).then_some(best)
}

pub(crate) fn apply_stone_fallback(manager: &mut SummonBuddyManager) {
    if !SUMMON_ANYWHERE.load(Ordering::Relaxed) {
        return;
    }
    release_used_monument(manager);
    let found = manager.buddy_stone_entity_id;
    let last = LAST_STONE.load(Ordering::Relaxed);
    if found != 0 {
        if found != last {
            LAST_STONE.store(found, Ordering::Relaxed);
        }
        return;
    }
    let Some(stone) = nearest_stone().or((last != 0).then_some(last)) else {
        return;
    };
    manager.buddy_stone_entity_id = stone;
    if STONE_FALLBACK_LOGGED.swap(stone, Ordering::Relaxed) != stone {
        log_line(format_args!(
            "summon anywhere: no monument found here; using monument {stone}"
        ));
    }
}

// The summon_buddy ChrSet (spirits and Torrent) already holds 80 entries, so
// it is not what stops a summon at about 10 bodies (r140 test: Lone Wolf
// Ashes gave 1 wolf of 3 with 9 bodies out, Black Knife Tiche none with 10).
// Moving it to a larger array of our own corrupted the heap on the next map
// load (c0000374) and was dropped.

/// Logs once how many bodies each spirit ash summons (BuddyParam rows per
/// trigger effect), to check them against what appears in the game.
static ASH_BODIES_LOGGED: AtomicBool = AtomicBool::new(false);

pub(crate) fn log_ash_bodies(manager: &SummonBuddyManager, set_capacity: u32) {
    if ASH_BODIES_LOGGED.load(Ordering::Relaxed) || manager.trigger_speffect_to_buddy_map.len() == 0 {
        return;
    }
    ASH_BODIES_LOGGED.store(true, Ordering::Relaxed);
    let mut bodies: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
    for (effect, buddy) in manager.trigger_speffect_to_buddy_map.iter() {
        if *effect > 0 && *buddy > 0 {
            *bodies.entry(*effect).or_default() += 1;
        }
    }
    let multi: Vec<String> = bodies
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(effect, n)| format!("{effect}x{n}"))
        .collect();
    log_line(format_args!(
        "spirit ash bodies: set capacity {set_capacity} (spirits from slot 20); {} ashes, {} bodies in all; more than one: {}",
        bodies.len(),
        bodies.values().sum::<usize>(),
        multi.join(" ")
    ));
}

/// The game refuses a summon at the monument its last summon came from
/// (active_summmon_buddy_stone_entity_id, +0x3C, equal to the one found,
/// +0x38) once none of those spirits is alive, until you rest: the item is not
/// even used (r140 test at Radahn: three Black Knife Tiche died, then no Ash
/// could be summoned, both monuments 1051390100). With Summon anywhere the
/// link is released while no spirit is alive and nothing is being summoned or
/// dismissed.
static MONUMENT_RELEASE_LOGGED: AtomicU32 = AtomicU32::new(0);

fn release_used_monument(manager: &mut SummonBuddyManager) {
    let used = manager.active_summmon_buddy_stone_entity_id;
    if used == 0 || manager.request_summon_speffect_id > 0 || manager.disappear_requested {
        return;
    }
    let any_alive = manager
        .groups
        .iter()
        .flat_map(|entry| entry.second.iter())
        .any(|group| chr_alive(unsafe { group.chr_ins.as_ref() }));
    if any_alive {
        return;
    }
    manager.active_summmon_buddy_stone_entity_id = 0;
    if MONUMENT_RELEASE_LOGGED.swap(used, Ordering::Relaxed) != used {
        log_line(format_args!(
            "summon anywhere: no spirit alive; monument {used} released for the next summon"
        ));
    }
}

/// Spirit ash cost (%) for the standalone DLL (in NativeTweaks the Hub's own
/// slider does it). Scales consumeMP and consumeHP of every Spirit Ash item
/// once, as soon as their rows are known and before the extra-cost and
/// low-FP passes read them, so those start from the scaled cost.
static ASH_COST_PERCENT: AtomicU32 = AtomicU32::new(100);
static ASH_COST_APPLIED: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_set_ash_cost_percent(percent: u32) {
    ASH_COST_PERCENT.store(percent.min(300), Ordering::Release);
}

pub(crate) fn update_ash_cost(state: &RuntimeState) {
    let percent = ASH_COST_PERCENT.load(Ordering::Acquire);
    if percent == 100
        || ASH_COST_APPLIED.load(Ordering::Acquire)
        || state.summon_dialog_originals.is_empty()
    {
        return;
    }
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let scale = |value: i16| {
        (f32::from(value) * percent as f32 / 100.0)
            .round()
            .clamp(0.0, f32::from(i16::MAX)) as i16
    };
    let mut rows = 0usize;
    for goods_id in state.summon_dialog_originals.keys() {
        if let Some(row) = repository.get_mut::<EquipParamGoods>(*goods_id as u32) {
            row.set_consume_mp(scale(row.consume_mp()));
            row.set_consume_hp(scale(row.consume_hp()));
            rows += 1;
        }
    }
    ASH_COST_APPLIED.store(true, Ordering::Release);
    log_line(format_args!("spirit ash cost: {percent}% on {rows} Spirit Ash items"));
}
