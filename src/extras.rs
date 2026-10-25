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
/// anywhere = activateRange 0xFFFF, no region, overwriteReturnRange -1 (no
/// dismissal for walking away); keep after boss = eliminateTargetEntityId 0;
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
    log_line(format_args!(
        "summoning monument rules: rows={rows} summon_anywhere={} keep_after_boss={} resummon_without_rest={}",
        wanted.anywhere, wanted.keep_after_boss, wanted.resummon_without_rest
    ));
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
