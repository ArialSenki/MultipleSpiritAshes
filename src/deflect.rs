//! NativeTweaks: longer Deflecting Hardtear perfect guard (Hub slider
//! DeflectWindowPercent). The guard-raise animations (c0000 TAE 19700-19735)
//! put SpEffect 102001 on the player with event type 66, which takes it off
//! again when the event ends: 6 frames, then 3, 2 and none after deflects in a
//! row. The player's HKS deflects a blocked hit only while that exact id is on
//! (env(1116, 102001), or the failsafes 102011/102013/102015), so a copy in
//! another row does nothing (in-game test 2026-10-28). Instead the row gets
//! the extra time as its effectEndurance and, when the animation takes it
//! off, it is put back on the player; when that one runs out it stays off.
//! Built into NativeTweaks.dll whether or not Multiple Spirit Ashes is active.

use super::*;

const WINDOW_SP_EFFECT_ID: i32 = 102001;
/// Deflecting Hardtear's main effect (300 s), for the log.
const HARDTEAR_SP_EFFECT_ID: i32 = 102000;
/// Our copy is taken as run out from this much before its full time on.
const EXPIRY_SLACK_MS: u64 = 50;
const MAX_DIAGNOSTICS: u32 = 40;

/// Extra time in milliseconds, 0 = off.
static EXTRA_MS: AtomicU32 = AtomicU32::new(0);
/// effectEndurance last written to the row (ms), u32::MAX = not written yet.
static ROW_MS: AtomicU32 = AtomicU32::new(u32::MAX);
static ORIGINAL_ENDURANCE: OnceLock<f32> = OnceLock::new();
static WINDOW_SEEN: AtomicBool = AtomicBool::new(false);
static DIAGNOSTICS: AtomicU32 = AtomicU32::new(0);
static HARDTEAR_LOGGED: AtomicBool = AtomicBool::new(false);
/// When we last put the window back on, for telling our own expiry apart.
static REAPPLIED_AT: Mutex<Option<Instant>> = Mutex::new(None);
static INSTALL: Once = Once::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Sets the extra perfect-guard time. Returns 1 when the frame task runs, 0
/// when the game's task scheduler isn't there yet (the Hub asks again).
#[unsafe(no_mangle)]
pub extern "C" fn nt_set_deflect_window(extra_ms: u32) -> u8 {
    EXTRA_MS.store(extra_ms, Ordering::Release);
    INSTALL.call_once(|| {
        let Ok(task_imp) = (unsafe { CSTaskImp::instance() }) else {
            return;
        };
        let task = task_imp.run_recurring(deflect_task as fn(&FD4TaskData), CSTaskGroupIndex::FrameBegin);
        std::mem::forget(task);
        INSTALLED.store(true, Ordering::Release);
    });
    u8::from(INSTALLED.load(Ordering::Acquire) || extra_ms == 0)
}

fn deflect_task(_: &FD4TaskData) {
    // A panic must not unwind into the game's task loop.
    let _ = std::panic::catch_unwind(update_deflect);
}

fn update_deflect() {
    let extra_ms = EXTRA_MS.load(Ordering::Acquire);
    if ROW_MS.load(Ordering::Acquire) != extra_ms && !write_row(extra_ms) {
        return;
    }
    if extra_ms == 0 {
        WINDOW_SEEN.store(false, Ordering::Release);
        return;
    }
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = world_chr_man.main_player.as_mut() else {
        WINDOW_SEEN.store(false, Ordering::Release);
        return;
    };
    let (mut window, mut hardtear) = (false, false);
    for entry in player.chr_ins.special_effect.as_ref().entries() {
        match entry.param_id {
            WINDOW_SP_EFFECT_ID => window = true,
            HARDTEAR_SP_EFFECT_ID => hardtear = true,
            _ => {}
        }
    }
    if hardtear && !HARDTEAR_LOGGED.swap(true, Ordering::AcqRel) {
        log_line(format_args!("deflect window: Deflecting Hardtear active on the player"));
    }
    let was = WINDOW_SEEN.swap(window, Ordering::AcqRel);
    if !was || window {
        return;
    }
    let Ok(mut reapplied) = REAPPLIED_AT.lock() else {
        return;
    };
    // Gone: either the animation event ended (put it back) or our own copy ran
    // out (leave it off). A new guard raise during our copy ends early.
    let ours_ran_out = reapplied
        .is_some_and(|at| at.elapsed().as_millis() as u64 + EXPIRY_SLACK_MS >= u64::from(extra_ms));
    if ours_ran_out {
        *reapplied = None;
        diagnostic(format_args!("deflect window: extra {extra_ms} ms ran out"));
        return;
    }
    player.apply_speffect(WINDOW_SP_EFFECT_ID, true);
    *reapplied = Some(Instant::now());
    diagnostic(format_args!("deflect window: guard window ended, held {extra_ms} ms more"));
}

/// The first lines are enough to see the window being carried.
fn diagnostic(message: std::fmt::Arguments) {
    if DIAGNOSTICS.fetch_add(1, Ordering::AcqRel) < MAX_DIAGNOSTICS {
        log_line(message);
    }
}

/// Gives row 102001 the extra time as its duration (the animation's type-66
/// event ignores it), or its own value back at 0.
fn write_row(extra_ms: u32) -> bool {
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return false;
    };
    let Some(row) = repository.get_mut::<SpEffectParam>(WINDOW_SP_EFFECT_ID as u32) else {
        return false;
    };
    let original = *ORIGINAL_ENDURANCE.get_or_init(|| row.effect_endurance());
    row.set_effect_endurance(if extra_ms == 0 { original } else { extra_ms as f32 / 1000.0 });
    ROW_MS.store(extra_ms, Ordering::Release);
    log_line(format_args!("deflect window: SpEffect {WINDOW_SP_EFFECT_ID} lasts {extra_ms} ms after the guard window"));
    true
}
