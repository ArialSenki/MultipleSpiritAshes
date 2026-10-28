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
static INSTALL: Once = Once::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Turns the cleaning on or off. Returns 1 when the frame task runs, 0 when
/// the game's task scheduler isn't there yet (the Hub asks again).
#[unsafe(no_mangle)]
pub extern "C" fn nt_set_player_clean(enabled: u8) -> u8 {
    ENABLED.store(enabled != 0, Ordering::Release);
    if enabled == 0 {
        return 1;
    }
    INSTALL.call_once(|| {
        let Ok(task_imp) = (unsafe { CSTaskImp::instance() }) else {
            return;
        };
        let task = task_imp.run_recurring(clean_task as fn(&FD4TaskData), CSTaskGroupIndex::FrameBegin);
        std::mem::forget(task);
        INSTALLED.store(true, Ordering::Release);
    });
    u8::from(INSTALLED.load(Ordering::Acquire))
}

fn clean_task(_: &FD4TaskData) {
    // A panic must not unwind into the game's task loop.
    let _ = std::panic::catch_unwind(update_clean);
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
