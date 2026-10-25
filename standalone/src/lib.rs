//! Standalone Multiple Spirit Ashes: DllMain reads MultipleSpiritAshes.ini
//! next to the DLL and starts the shared code (`multiple_spirit_ashes`, the
//! same code NativeTweaks.dll links in). An optional key dismisses every Ash.

use std::{
    ffi::{OsString, c_void},
    fs,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    thread,
    time::Duration,
};

use multiple_spirit_ashes::{
    NtMsaSettings, nt_msa_dismiss_all, nt_msa_set_ash_cost_percent, nt_msa_set_buddy_stone_rules,
    nt_msa_start,
};
use windows::Win32::{
    Foundation::{HINSTANCE, HMODULE},
    System::{LibraryLoader::GetModuleFileNameW, Threading::GetCurrentProcessId},
    UI::{
        Input::KeyboardAndMouse::GetAsyncKeyState,
        WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    },
};

const DLL_PROCESS_ATTACH: u32 = 1;
const CONFIG_FILE_NAME: &str = "MultipleSpiritAshes.ini";
const KEY_POLL: Duration = Duration::from_millis(50);

#[unsafe(no_mangle)]
/// # Safety
/// Called by Windows when this DLL is attached to the game process.
pub unsafe extern "system" fn DllMain(hmodule: HINSTANCE, reason: u32, _: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        let module = hmodule.0 as isize;
        thread::spawn(move || run(module));
    }
    1
}

struct Config {
    enabled: bool,
    max_active_ashes: u32,
    ash_cost_percent: u32,
    extra_cost_percent: u32,
    damage_reduction_percent: u32,
    hud_rows: bool,
    summon_anywhere: bool,
    keep_ashes_after_boss: bool,
    resummon_without_rest: bool,
    dismiss_all_key: Option<i32>,
    log_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            max_active_ashes: 8,
            ash_cost_percent: 100,
            extra_cost_percent: 100,
            damage_reduction_percent: 0,
            hud_rows: true,
            summon_anywhere: true,
            keep_ashes_after_boss: true,
            resummon_without_rest: true,
            dismiss_all_key: None,
            log_enabled: false,
        }
    }
}

fn run(module: isize) {
    let config = module_dir(module)
        .and_then(|dir| fs::read_to_string(dir.join(CONFIG_FILE_NAME)).ok())
        .map(|text| parse_config(&text))
        .unwrap_or_default();
    // Behaviour the Hub keeps always on is always on here too.
    let settings = NtMsaSettings {
        enabled: u8::from(config.enabled),
        max_active_ashes: config.max_active_ashes,
        log_enabled: u8::from(config.log_enabled),
        toggle_dismiss: 1,
        suppress_prompts: 1,
        follow_areas: 1,
        fast_dismissal: 1,
        low_fp_dismissal: 1,
        hud_rows: u8::from(config.hud_rows),
        extra_cost_percent: config.extra_cost_percent,
        damage_reduction_percent: config.damage_reduction_percent,
    };
    nt_msa_set_ash_cost_percent(config.ash_cost_percent);
    nt_msa_set_buddy_stone_rules(
        u8::from(config.summon_anywhere),
        u8::from(config.keep_ashes_after_boss),
        u8::from(config.resummon_without_rest),
    );
    if unsafe { nt_msa_start(module as *mut c_void, &settings) } == 0 {
        return;
    }
    let Some(key) = config.dismiss_all_key else {
        return;
    };
    let mut was_down = false;
    loop {
        thread::sleep(KEY_POLL);
        let down = game_has_focus() && unsafe { GetAsyncKeyState(key) } as u16 & 0x8000 != 0;
        if down && !was_down {
            nt_msa_dismiss_all();
        }
        was_down = down;
    }
}

/// Only the game's own window counts, so the key is free in other programs.
fn game_has_focus() -> bool {
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return false;
    }
    let mut process = 0u32;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
    process == unsafe { GetCurrentProcessId() }
}

fn module_dir(module: isize) -> Option<PathBuf> {
    let mut buffer = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(HMODULE(module as *mut _)), &mut buffer) };
    if len == 0 || len as usize >= buffer.len() {
        return None;
    }
    PathBuf::from(OsString::from_wide(&buffer[..len as usize]))
        .parent()
        .map(PathBuf::from)
}

fn parse_config(text: &str) -> Config {
    let mut config = Config::default();
    for line in text.lines() {
        let line = line.split([';', '#']).next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "enabled" => config.enabled = parse_bool(value).unwrap_or(config.enabled),
            "maxactiveashes" => {
                if let Ok(parsed) = value.parse::<u32>() {
                    config.max_active_ashes = parsed.clamp(2, 20);
                }
            }
            "ashcostpercent" => {
                if let Ok(parsed) = value.parse::<u32>() {
                    config.ash_cost_percent = parsed.min(300);
                }
            }
            "extraashcostpercent" => {
                if let Ok(parsed) = value.parse::<u32>() {
                    config.extra_cost_percent = parsed.min(200);
                }
            }
            "damagelossperextraashpercent" => {
                if let Ok(parsed) = value.parse::<u32>() {
                    config.damage_reduction_percent = parsed.min(20);
                }
            }
            "hudrows" => config.hud_rows = parse_bool(value).unwrap_or(config.hud_rows),
            "summonanywhere" => {
                config.summon_anywhere = parse_bool(value).unwrap_or(config.summon_anywhere)
            }
            "keepashesafterboss" => {
                config.keep_ashes_after_boss =
                    parse_bool(value).unwrap_or(config.keep_ashes_after_boss)
            }
            "resummonwithoutrest" => {
                config.resummon_without_rest =
                    parse_bool(value).unwrap_or(config.resummon_without_rest)
            }
            "dismissallkey" => config.dismiss_all_key = parse_key(value),
            "logenabled" => config.log_enabled = parse_bool(value).unwrap_or(config.log_enabled),
            _ => {}
        }
    }
    config
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// F1-F24, a letter or digit, or a virtual-key code (0x74). Empty = no key.
fn parse_key(value: &str) -> Option<i32> {
    let value = value.trim().to_ascii_uppercase();
    if value.is_empty() || value == "NONE" {
        return None;
    }
    if let Some(number) = value.strip_prefix('F').and_then(|n| n.parse::<i32>().ok()) {
        return (1..=24).contains(&number).then_some(0x70 + number - 1);
    }
    if let Some(hex) = value.strip_prefix("0X") {
        return i32::from_str_radix(hex, 16).ok().filter(|code| (1..=0xFE).contains(code));
    }
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c as i32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_values_are_read_and_clamped() {
        let config = parse_config(
            "[MultipleSpiritAshes]\nEnabled=true\nMaxActiveAshes=50\nAshCostPercent=400\nExtraAshCostPercent=50\n\
             DamageLossPerExtraAshPercent=99\nHudRows=false\nSummonAnywhere=no\n\
             KeepAshesAfterBoss=yes\nResummonWithoutRest=on\nDismissAllKey=F5\nLogEnabled=1\n",
        );
        assert!(config.enabled);
        assert_eq!(config.max_active_ashes, 20);
        assert_eq!(config.ash_cost_percent, 300);
        assert_eq!(config.extra_cost_percent, 50);
        assert_eq!(config.damage_reduction_percent, 20);
        assert!(!config.hud_rows);
        assert!(!config.summon_anywhere);
        assert!(config.keep_ashes_after_boss);
        assert!(config.resummon_without_rest);
        assert_eq!(config.dismiss_all_key, Some(0x74));
        assert!(config.log_enabled);
    }

    #[test]
    fn keys_parse() {
        assert_eq!(parse_key("f1"), Some(0x70));
        assert_eq!(parse_key("G"), Some('G' as i32));
        assert_eq!(parse_key("0x74"), Some(0x74));
        assert_eq!(parse_key(""), None);
        assert_eq!(parse_key("F25"), None);
    }
}
