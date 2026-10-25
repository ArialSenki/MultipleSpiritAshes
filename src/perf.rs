//! NativeTweaks: per-task timing for the built-in Multiple Spirit Ashes,
//! written to the log every 30 s while logging is on (like the Hub's PerfLog).

use super::*;

pub(crate) const POLICY: usize = 0;
pub(crate) const ELIGIBILITY: usize = 1;
pub(crate) const HUD: usize = 2;
const NAMES: [&str; 3] = ["policy", "eligibility", "hud"];
const REPORT_INTERVAL: Duration = Duration::from_secs(30);

static CALLS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static TOTAL_NS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static MAX_NS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static LAST_REPORT: Mutex<Option<Instant>> = Mutex::new(None);

/// Runs `task`, adding its time to `slot` when logging is on.
pub(crate) fn measure(slot: usize, task: impl FnOnce()) {
    if !settings().log_enabled {
        task();
        return;
    }
    let started = Instant::now();
    task();
    let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    CALLS[slot].fetch_add(1, Ordering::Relaxed);
    TOTAL_NS[slot].fetch_add(elapsed, Ordering::Relaxed);
    MAX_NS[slot].fetch_max(elapsed, Ordering::Relaxed);
    if slot == POLICY {
        report_if_due();
    }
}

fn report_if_due() {
    let Ok(mut last) = LAST_REPORT.lock() else {
        return;
    };
    let now = Instant::now();
    let Some(previous) = *last else {
        *last = Some(now);
        return;
    };
    if now.saturating_duration_since(previous) < REPORT_INTERVAL {
        return;
    }
    *last = Some(now);
    let mut parts = Vec::with_capacity(NAMES.len());
    for (slot, name) in NAMES.iter().enumerate() {
        let calls = CALLS[slot].swap(0, Ordering::Relaxed);
        let total = TOTAL_NS[slot].swap(0, Ordering::Relaxed);
        let max = MAX_NS[slot].swap(0, Ordering::Relaxed);
        let average_us = if calls > 0 { total as f64 / calls as f64 / 1000.0 } else { 0.0 };
        parts.push(format!(
            "{name}: calls={calls} avg_us={average_us:.1} max_us={:.1} total_ms={:.1}",
            max as f64 / 1000.0,
            total as f64 / 1_000_000.0
        ));
    }
    log_line(format_args!("perf (30 s): {}", parts.join(" | ")));
}
