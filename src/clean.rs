//! NativeTweaks: keep your character clean (Hub option KeepPlayerClean). The
//! game's Soap (SpEffect 502120) cleans the player when it ends: its
//! effectEndDeleteDecalGroupId 10 removes the paint decals (blood, mud,
//! scorch marks) of that group from the character. Row 102006 (Deflecting
//! Hardtear - Unused) becomes a bare copy of that one job: 0.1 s, no bubbles,
//! nothing chained; it is put on the player every quarter second, so marks
//! from a hit last at most that long. Enemies keep theirs. Setting the
//! player's NpcParam paintRenderTargetSize to 0 did nothing in game
//! (2026-10-28): the player's paint target doesn't come from it.
//! Built into NativeTweaks.dll whether or not Multiple Spirit Ashes is active.

use super::*;

const CARRIER_SP_EFFECT_ID: i32 = 102006;
/// The Soap's decal group (SpEffect 502120).
const SOAP_DECAL_GROUP: i16 = 10;
const CARRIER_SECONDS: f32 = 0.1;
const INTERVAL: Duration = Duration::from_millis(250);

static ENABLED: AtomicBool = AtomicBool::new(false);
static CARRIER_READY: AtomicBool = AtomicBool::new(false);
static LAST_APPLIED: Mutex<Option<Instant>> = Mutex::new(None);
static INSTALL: Mutex<()> = Mutex::new(());
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// A SpEffect the Hub wants on the player all the time (Damage taken, r157:
/// its own SpEffectParam row with the incoming damage rates); 0 = none.
static KEPT_EFFECT: AtomicI32 = AtomicI32::new(0);
/// The one last put on the player, to take it off when the Hub stops.
static KEPT_APPLIED: AtomicI32 = AtomicI32::new(0);
static KEPT_CHECKED: Mutex<Option<Instant>> = Mutex::new(None);

/// Turns the cleaning on or off. Returns 1 when the frame task runs, 0 when
/// the game's task scheduler isn't there yet (the Hub asks again).
#[unsafe(no_mangle)]
pub extern "C" fn nt_set_player_clean(enabled: u8) -> u8 {
    ENABLED.store(enabled != 0, Ordering::Release);
    if enabled == 0 {
        return 1;
    }
    // On an unknown game version the bindings panic when the task is
    // registered, and that would abort the game inside this extern "C"
    // function (audit 2026-11-08).
    if supported_version(Program::current()).is_none() {
        return 0;
    }
    crate::game_menus::install_frame_task(&INSTALL, &INSTALLED, clean_task);
    u8::from(INSTALLED.load(Ordering::Acquire))
}

/// Keeps SpEffect `id` on the player (checked every quarter second, so it
/// comes back after death or a load); 0 takes the previous one off. Returns
/// 1 when the frame task runs, 0 when it can't run yet (ask again).
#[unsafe(no_mangle)]
pub extern "C" fn nt_keep_player_effect(id: i32) -> u8 {
    KEPT_EFFECT.store(id.max(0), Ordering::Release);
    if supported_version(Program::current()).is_none() {
        return 0;
    }
    crate::game_menus::install_frame_task(&INSTALL, &INSTALLED, clean_task);
    u8::from(INSTALLED.load(Ordering::Acquire))
}

fn clean_task(_: &FD4TaskData) {
    // A panic must not unwind into the game's task loop.
    let _ = std::panic::catch_unwind(update_clean);
    let _ = std::panic::catch_unwind(update_kept_effect);
}

fn update_kept_effect() {
    let want = KEPT_EFFECT.load(Ordering::Acquire);
    let applied = KEPT_APPLIED.load(Ordering::Acquire);
    if want == 0 && applied == 0 {
        return;
    }
    let mut checked = KEPT_CHECKED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if want == applied && checked.is_some_and(|at| at.elapsed() < INTERVAL) {
        return;
    }
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = world_chr_man.main_player.as_mut() else {
        return;
    };
    let has_applied = applied != 0
        && player.chr_ins.special_effect.as_ref().entries().any(|entry| entry.param_id == applied);
    if applied != want && has_applied {
        player.remove_speffect(applied);
        log_line(format_args!("kept player effect: SpEffect {applied} removed"));
    }
    let has_wanted =
        want != 0 && player.chr_ins.special_effect.as_ref().entries().any(|entry| entry.param_id == want);
    if want != 0 && !has_wanted {
        player.apply_speffect(want, true);
        if applied != want {
            log_line(format_args!("kept player effect: SpEffect {want} applied"));
        }
    }
    KEPT_APPLIED.store(want, Ordering::Release);
    *checked = Some(Instant::now());
}

fn update_clean() {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    if !CARRIER_READY.load(Ordering::Acquire) && !write_carrier() {
        return;
    }
    let Ok(mut last) = LAST_APPLIED.lock() else {
        return;
    };
    if last.is_some_and(|at| at.elapsed() < INTERVAL) {
        return;
    }
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = world_chr_man.main_player.as_mut() else {
        return;
    };
    let carried = player
        .chr_ins
        .special_effect
        .as_ref()
        .entries()
        .any(|entry| entry.param_id == CARRIER_SP_EFFECT_ID);
    if !carried {
        player.apply_speffect(CARRIER_SP_EFFECT_ID, true);
    }
    *last = Some(Instant::now());
}

/// Row 102006 only lasts 0.1 s and deletes the Soap's decal group when it
/// ends; everything else stays as the unused row has it (no effect).
fn write_carrier() -> bool {
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return false;
    };
    let Some(carrier) = repository.get_mut::<SpEffectParam>(CARRIER_SP_EFFECT_ID as u32) else {
        return false;
    };
    carrier.set_effect_endurance(CARRIER_SECONDS);
    carrier.set_effect_end_delete_decal_group_id(SOAP_DECAL_GROUP);
    CARRIER_READY.store(true, Ordering::Release);
    log_line(format_args!(
        "player clean: SpEffect {CARRIER_SP_EFFECT_ID} deletes decal group {SOAP_DECAL_GROUP} every {} ms",
        INTERVAL.as_millis()
    ));
    true
}
