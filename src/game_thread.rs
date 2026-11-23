//! One call at a time run inside the game's own task loop (21-11).
//!
//! NativeTweaks' Enemies stay dead worker used to call the game's character
//! enable/disable setters from its own thread. They do a plain
//! read-modify-write of ChrIns+0x539 (and touch the character it rides), so a
//! frame that changed the same byte at that moment could lose a bit. Now the
//! worker hands the call to the GameMan step and waits for it.
//!
//! The worker's call is in SLOT until a game task takes it; a call the game
//! has not taken when the wait ends is taken back and never runs, so the
//! worker's stack data it points to is never used late.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
};
use std::time::{Duration, Instant};

/// The call: returns 0 or 1 (passed back to the waiting thread).
pub type GameThreadFn = unsafe extern "C" fn(usize) -> u8;

struct Pending {
    function: GameThreadFn,
    argument: usize,
    ticket: u64,
}

static SLOT: Mutex<Option<Pending>> = Mutex::new(None);
static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);
static DONE_TICKET: AtomicU64 = AtomicU64::new(0);
static DONE_RESULT: AtomicU8 = AtomicU8::new(0);
/// The task that drains SLOT is registered (nt_msa_start ran).
pub(crate) static READY: AtomicBool = AtomicBool::new(false);
/// Only one waiting caller at a time.
static CALLER: Mutex<()> = Mutex::new(());

/// Runs the pending call, if any. Called from a game task every frame.
pub(crate) fn drain() {
    let pending = match SLOT.try_lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => return,
    };
    if let Some(Pending { function, argument, ticket }) = pending {
        let result = unsafe { function(argument) };
        DONE_RESULT.store(result, Ordering::Release);
        DONE_TICKET.store(ticket, Ordering::Release);
    }
}

/// 1 once the draining task is registered.
#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_game_thread_ready() -> u8 {
    u8::from(READY.load(Ordering::Acquire))
}

/// Runs `function(argument)` in the game's task loop and returns 1 + its
/// result (1 or 2), or 0 when it did not run: no task yet, or the game did not
/// take it within `timeout_ms` (loading screens pause the task loop).
#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_on_game_thread(function: GameThreadFn, argument: usize, timeout_ms: u32) -> u8 {
    if !READY.load(Ordering::Acquire) {
        return 0;
    }
    let Ok(_caller) = CALLER.lock() else {
        return 0;
    };
    let ticket = NEXT_TICKET.fetch_add(1, Ordering::AcqRel);
    match SLOT.lock() {
        Ok(mut slot) => *slot = Some(Pending { function, argument, ticket }),
        Err(_) => return 0,
    }
    let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms.clamp(1, 5000)));
    loop {
        if DONE_TICKET.load(Ordering::Acquire) == ticket {
            return 1 + DONE_RESULT.load(Ordering::Acquire).min(1);
        }
        if Instant::now() >= deadline {
            // Take it back if the game has not started it.
            if let Ok(mut slot) = SLOT.lock() {
                if slot.as_ref().is_some_and(|p| p.ticket == ticket) {
                    *slot = None;
                    return 0;
                }
            }
            // Taken: it is running now; wait for it to finish.
            while DONE_TICKET.load(Ordering::Acquire) != ticket {
                std::thread::sleep(Duration::from_millis(1));
            }
            return 1 + DONE_RESULT.load(Ordering::Acquire).min(1);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn add_one(argument: usize) -> u8 {
        let counter = unsafe { &*(argument as *const AtomicU64) };
        counter.fetch_add(1, Ordering::AcqRel);
        1
    }

    #[test]
    fn runs_once_on_the_draining_thread_or_not_at_all() {
        READY.store(true, Ordering::Release);
        let counter = AtomicU64::new(0);
        // Nothing drains: taken back, never runs.
        assert_eq!(nt_msa_on_game_thread(add_one, &counter as *const _ as usize, 20), 0);
        drain();
        assert_eq!(counter.load(Ordering::Acquire), 0);
        // A draining "game" thread: runs exactly once.
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let game = std::thread::spawn(move || {
            while !stop2.load(Ordering::Acquire) {
                drain();
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        assert_eq!(nt_msa_on_game_thread(add_one, &counter as *const _ as usize, 1000), 2);
        stop.store(true, Ordering::Release);
        game.join().unwrap();
        assert_eq!(counter.load(Ordering::Acquire), 1);
        READY.store(false, Ordering::Release);
    }
}
