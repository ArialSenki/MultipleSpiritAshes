//! Compact Spirit Ash rows on the HUD.
//!
//! The rows are the game's own off-screen ally tags (`RemotePCTag0..5`),
//! which `hud_movie` stacks under the hidden Spirit Ash panel. The game draws
//! them from `CSFeManImp::friendly_chr_tag_displays`: every frame it rebuilds
//! those entries from the real allies (empty ones are reset), then copies them
//! into the HUD values (game function at `fe_friend_tag_copy_function`), and
//! the HUD stacks every entry marked off-screen at the next free tag position.
//! Writing the entries from a task therefore lost them (rebuilt before the
//! copy), so the rows are written into free entries right before that copy,
//! from a hook on its call, and taken out again right after it. Each entry is
//! off-screen and not visible (the visible path places the tag over a
//! character at `screen_pos`).
//!
//! One row per Ash, oldest first: the Ash item's name (GoodsName) and its HP;
//! an Ash with several spirits shows their summed HP and "×N"; an Ash being
//! dismissed shows its name on the dimmed line; when the Ashes do not fit,
//! the last row reads "+N" with the HP of the rest.

use std::{
    collections::HashMap,
    ffi::c_void,
    ptr,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use eldenring::{
    cs::{
        BuddyParam, CSFeManImp, ChrFriendTagEntry, FieldInsHandle, MsgRepositoryImp, NpcParam,
        SoloParamRepository, SummonBuddyManager, WorldChrMan,
    },
    fd4::FD4TaskData,
};
use fromsoftware_shared::{FromStatic, program::Program};
use pelite::pe64::Pe;

use super::hud_movie::DIMMED_NAMES_READY;

/// Friendly entries with a tag in the HUD movie (the game has seven entries,
/// the movie six tags).
const ROW_SLOTS: usize = 6;
const FALLBACK_NAME: &str = "Spirit";
/// MsgRepository categories the game tries, in order, for a goods name and
/// for an NPC name (base, patch and DLC message files).
const GOODS_NAME_CATEGORIES: [u32; 4] = [111, 10, 319, 419];
const NPC_NAME_CATEGORIES: [u32; 4] = [119, 18, 328, 428];
/// LookupEntry starts with its two bounds checks (`cmp edx,[rcx+10h]; jnb`,
/// `cmp r8d,[rcx+14h]; jnb`).
const MSG_LOOKUP_PROLOGUE: [u8; 4] = [0x3b, 0x51, 0x10, 0x73];
const MSG_LOOKUP_SECOND_CHECK: [u8; 5] = [0x44, 0x3b, 0x41, 0x14, 0x73];

type TagCopyFn = unsafe extern "system" fn(*mut CSFeManImp, f32);
type MsgLookupFn = unsafe extern "system" fn(*mut c_void, u32, u32, i32) -> *const u16;

static ORIGINAL_TAG_COPY: AtomicUsize = AtomicUsize::new(0);
static MSG_LOOKUP: AtomicUsize = AtomicUsize::new(0);
/// Free entries found by the last copy (for the log).
static LAST_FREE_SLOTS: AtomicUsize = AtomicUsize::new(ROW_SLOTS);

static HUD_STATE: LazyLock<Mutex<HudState>> = LazyLock::new(|| Mutex::new(HudState::default()));

#[derive(Default)]
struct HudState {
    /// Effects of the living Ashes, oldest first.
    order: Vec<i32>,
    names: HashMap<i32, String>,
    /// What the next copies show (written by the task, read by the hook).
    ashes: Vec<AshSummary>,
    last_status: String,
}

#[derive(Clone, Debug, PartialEq)]
struct AshSummary {
    effect: i32,
    name: String,
    spirits: usize,
    hp: u32,
    max_hp: u32,
    max_uncapped_hp: u32,
    dismissing: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct HudRow {
    name: String,
    hp: u32,
    max_hp: u32,
    max_uncapped_hp: u32,
    dimmed: bool,
}

struct AshHudMember {
    npc_name_id: i32,
    hp: u32,
    max_hp: u32,
    max_uncapped_hp: u32,
    dismissing: bool,
}

/// Installs the hook on the tag copy call and resolves the message lookup.
/// Both are checked against the expected code first; without the hook the
/// rows stay hidden (the task keeps running and only logs).
pub(crate) fn install_native_hooks() {
    let rvas = super::native_rvas();
    let program = Program::current();

    if let Ok(lookup) = program.rva_to_va(rvas.msg_lookup_function) {
        let code = unsafe { std::slice::from_raw_parts(lookup as *const u8, 10) };
        if code[..4] == MSG_LOOKUP_PROLOGUE && code[5..10] == MSG_LOOKUP_SECOND_CHECK {
            MSG_LOOKUP.store(lookup as usize, Ordering::Release);
        } else {
            super::log_line(format_args!(
                "spirit ash HUD: message lookup signature mismatch at RVA {:#x}; rows use NPC names",
                rvas.msg_lookup_function
            ));
        }
    }

    let Ok(copy) = program.rva_to_va(rvas.fe_friend_tag_copy_function) else {
        super::log_line("spirit ash HUD: tag copy RVA is outside the executable; rows unavailable");
        return;
    };
    ORIGINAL_TAG_COPY.store(copy as usize, Ordering::Release);
    let installed = super::install_relative_branch_stub(
        rvas.fe_friend_tag_copy_call_site,
        rvas.fe_friend_tag_copy_function,
        0xe8,
        build_tag_copy_stub,
        "spirit ash HUD tag copy hook",
    );
    if !installed {
        ORIGINAL_TAG_COPY.store(0, Ordering::Release);
        super::log_line("spirit ash HUD: rows unavailable (tag copy hook not installed)");
    }
}

/// Jumps to the Rust detour with the call's own arguments (rcx = CSFeManImp,
/// xmm1 = frame time) and return address.
fn build_tag_copy_stub(_original_target: usize) -> Vec<u8> {
    let mut code = Vec::with_capacity(16);
    super::append_absolute_indirect_jump(&mut code, tag_copy_detour as *const () as usize);
    code
}

unsafe extern "system" fn tag_copy_detour(fe_manager: *mut CSFeManImp, delta: f32) {
    let original = ORIGINAL_TAG_COPY.load(Ordering::Acquire);
    let written = std::panic::catch_unwind(|| unsafe { insert_rows(fe_manager) })
        .unwrap_or([false; ROW_SLOTS]);
    if original != 0 {
        let original: TagCopyFn = unsafe { std::mem::transmute(original) };
        unsafe { original(fe_manager, delta) };
    }
    let _ = std::panic::catch_unwind(|| unsafe { remove_rows(fe_manager, &written) });
}

/// Writes the rows into the free friendly entries; returns which ones.
unsafe fn insert_rows(fe_manager: *mut CSFeManImp) -> [bool; ROW_SLOTS] {
    let mut written = [false; ROW_SLOTS];
    if fe_manager.is_null() {
        return written;
    }
    let Ok(state) = HUD_STATE.lock() else {
        return written;
    };
    if state.ashes.is_empty() {
        return written;
    }
    let displays = unsafe { &mut (*fe_manager).friendly_chr_tag_displays };
    let free = (0..ROW_SLOTS)
        .filter(|&slot| entry_is_free(&displays[slot]))
        .collect::<Vec<_>>();
    LAST_FREE_SLOTS.store(free.len(), Ordering::Relaxed);
    let dimmed_line = DIMMED_NAMES_READY.load(Ordering::Acquire);
    for (row, slot) in plan_rows(&state.ashes, free.len()).iter().zip(free) {
        let entry = &mut displays[slot];
        written[slot] = true;
        if !write_entry(entry, row, dimmed_line) {
            clear_entry(entry);
        }
    }
    written
}

unsafe fn remove_rows(fe_manager: *mut CSFeManImp, written: &[bool; ROW_SLOTS]) {
    if fe_manager.is_null() {
        return;
    }
    let displays = unsafe { &mut (*fe_manager).friendly_chr_tag_displays };
    for (slot, _) in written.iter().enumerate().filter(|(_, written)| **written) {
        clear_entry(&mut displays[slot]);
    }
}

/// An entry the game left empty this frame (its rebuild resets those).
fn entry_is_free(entry: &ChrFriendTagEntry) -> bool {
    entry.field_ins_handle.is_empty()
        && !entry.is_visible
        && !entry.is_not_on_screen
        && !entry.is_debug_summon
}

fn write_entry(entry: &mut ChrFriendTagEntry, row: &HudRow, dimmed_line: bool) -> bool {
    let (name, dimmed_name) = if row.dimmed && dimmed_line {
        ("", row.name.as_str())
    } else {
        (row.name.as_str(), "")
    };
    if entry.name_string.assign_str(name).is_err()
        || entry.role_string.assign_str(dimmed_name).is_err()
    {
        return false;
    }
    // Off-screen and not visible: the HUD stacks it at the next tag position.
    entry.is_visible = false;
    entry.is_line_of_sight_blocked = false;
    entry.is_not_on_screen = true;
    entry.role_name_color = 1; // white
    // No character handle: the copy would look the character up as a player.
    set_empty_handle(&mut entry.field_ins_handle);
    // The tag is drawn only while its max HP is above 0.
    entry.hp_max_uncapped = row.max_uncapped_hp.max(row.max_hp).max(1);
    entry.hp = row.hp;
    entry.max_recoverable_hp = row.hp;
    entry.max_hp = row.max_hp.max(1);
    entry.last_damage_time_delta = 0.0;
    entry.voice_chat_state = 0;
    entry.team_type = 0;
    entry.is_down_scaled = false;
    entry.has_rune_arc = false;
    entry.is_debug_summon = false;
    true
}

/// Back to how the game's rebuild leaves an unused entry.
fn clear_entry(entry: &mut ChrFriendTagEntry) {
    entry.is_visible = false;
    entry.is_line_of_sight_blocked = false;
    entry.is_not_on_screen = false;
    entry.role_name_color = 0;
    set_empty_handle(&mut entry.field_ins_handle);
    let _ = entry.name_string.assign_str("");
    let _ = entry.role_string.assign_str("");
    entry.hp_max_uncapped = 0;
    entry.hp = 0;
    entry.max_recoverable_hp = 0;
    entry.max_hp = 0;
    entry.last_damage_time_delta = 0.0;
    entry.voice_chat_state = 0;
    entry.team_type = 0;
    entry.is_down_scaled = false;
    entry.has_rune_arc = false;
    entry.is_debug_summon = false;
}

fn set_empty_handle(handle: &mut FieldInsHandle) {
    unsafe {
        (handle as *mut FieldInsHandle).cast::<u64>().write(u64::MAX);
    }
}

/// One row per Ash in `ashes` order; when they do not fit in `slots`, the
/// last row stands for the rest ("+N", their summed HP).
fn plan_rows(ashes: &[AshSummary], slots: usize) -> Vec<HudRow> {
    if slots == 0 || ashes.is_empty() {
        return Vec::new();
    }
    let shown = if ashes.len() <= slots {
        ashes.len()
    } else {
        slots - 1
    };
    let mut rows = ashes[..shown]
        .iter()
        .map(|ash| HudRow {
            name: if ash.spirits > 1 {
                format!("{} ×{}", ash.name, ash.spirits)
            } else {
                ash.name.clone()
            },
            hp: ash.hp,
            max_hp: ash.max_hp,
            max_uncapped_hp: ash.max_uncapped_hp,
            dimmed: ash.dismissing,
        })
        .collect::<Vec<_>>();
    let rest = &ashes[shown..];
    if !rest.is_empty() {
        let sum = |value: fn(&AshSummary) -> u32| {
            rest.iter().fold(0u32, |total, ash| total.saturating_add(value(ash)))
        };
        rows.push(HudRow {
            name: format!("+{}", rest.len()),
            hp: sum(|ash| ash.hp),
            max_hp: sum(|ash| ash.max_hp),
            max_uncapped_hp: sum(|ash| ash.max_uncapped_hp),
            dimmed: rest.iter().all(|ash| ash.dismissing),
        });
    }
    rows
}

pub(crate) fn independent_ash_hud_task(_: &FD4TaskData) {
    if !super::hud_rows_active() {
        return;
    }

    let groups = collect_live_ash_groups();
    let Ok(mut state) = HUD_STATE.lock() else {
        return;
    };
    let Some(groups) = groups else {
        state.ashes.clear();
        update_status(&mut state, "inactive:world_manager_unavailable".to_owned());
        return;
    };
    if groups.is_empty() {
        state.ashes.clear();
        update_status(&mut state, "inactive:no_living_ashes".to_owned());
        return;
    }

    let HudState { order, names, .. } = &mut *state;
    order.retain(|effect| groups.contains_key(effect));
    let mut new_effects = groups
        .keys()
        .filter(|effect| !order.contains(effect))
        .copied()
        .collect::<Vec<_>>();
    new_effects.sort_unstable();
    order.extend(new_effects);

    let ashes = order
        .iter()
        .filter_map(|effect| {
            let members = groups.get(effect)?;
            let name = ash_name(names, *effect, members);
            let sum = |value: fn(&AshHudMember) -> u32| {
                members
                    .iter()
                    .fold(0u32, |total, member| total.saturating_add(value(member)))
            };
            Some(AshSummary {
                effect: *effect,
                name,
                spirits: members.len(),
                hp: sum(|member| member.hp),
                max_hp: sum(|member| member.max_hp),
                max_uncapped_hp: sum(|member| member.max_uncapped_hp),
                dismissing: members.iter().any(|member| member.dismissing),
            })
        })
        .collect::<Vec<_>>();

    if !super::settings().log_enabled {
        state.ashes = ashes;
        return;
    }
    let hooked = ORIGINAL_TAG_COPY.load(Ordering::Acquire) != 0;
    let free_slots = LAST_FREE_SLOTS.load(Ordering::Relaxed);
    let signature = ashes
        .iter()
        .map(|ash| {
            format!(
                "{}:{}{}",
                ash.effect,
                ash.spirits,
                if ash.dismissing { ":dismissing" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let status = format!(
        "active:ashes={signature}:rows={}:free_slots={free_slots}:hooked={hooked}",
        plan_rows(&ashes, free_slots).len()
    );
    if state.last_status != status {
        super::log_line(format_args!("spirit ash HUD: {status}"));
        for ash in &ashes {
            super::log_line(format_args!(
                "spirit ash HUD row: effect={} name={} spirits={} hp={}/{} dismissing={}",
                ash.effect, ash.name, ash.spirits, ash.hp, ash.max_uncapped_hp, ash.dismissing
            ));
        }
        state.last_status = status;
    }
    state.ashes = ashes;
}

/// The Ash item's name (the summon effect ID is the goods ID of the Ash),
/// else the spirit's NPC name, else a placeholder. Found names are cached.
fn ash_name(names: &mut HashMap<i32, String>, effect: i32, members: &[AshHudMember]) -> String {
    if let Some(name) = names.get(&effect) {
        return name.clone();
    }
    let found = lookup_message(&GOODS_NAME_CATEGORIES, effect).or_else(|| {
        members
            .iter()
            .map(|member| member.npc_name_id)
            .find(|id| *id > 0)
            .and_then(|id| lookup_message(&NPC_NAME_CATEGORIES, id))
    });
    match found {
        Some(name) => {
            super::log_line(format_args!("spirit ash HUD name: effect={effect} name={name}"));
            names.insert(effect, name.clone());
            name
        }
        None => FALLBACK_NAME.to_owned(),
    }
}

fn lookup_message(categories: &[u32], id: i32) -> Option<String> {
    let lookup = MSG_LOOKUP.load(Ordering::Acquire);
    if lookup == 0 || id <= 0 {
        return None;
    }
    let repository = unsafe { MsgRepositoryImp::instance_mut().ok()? };
    let repository = repository as *mut MsgRepositoryImp as *mut c_void;
    let lookup: MsgLookupFn = unsafe { std::mem::transmute(lookup) };
    categories.iter().find_map(|&category| {
        let text = unsafe { lookup(repository, 0, category, id) };
        if text.is_null() {
            return None;
        }
        let length = (0..256).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        let name = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
        let name = name.trim();
        valid_name(name).then(|| name.to_owned())
    })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && !name.starts_with('?')
        && !name.chars().all(|character| character.is_ascii_digit())
        && !matches!(name, "NULL" | "null")
}

/// Living local spirits by summon effect.
fn collect_live_ash_groups() -> Option<HashMap<i32, Vec<AshHudMember>>> {
    let world = unsafe { WorldChrMan::instance_mut().ok()? };
    let manager_address = unsafe {
        ptr::addr_of!(world.summon_buddy_manager)
            .cast::<usize>()
            .read()
    };
    if manager_address == 0 {
        return None;
    }
    let manager = unsafe { &mut *(manager_address as *mut SummonBuddyManager) };

    let mut buddy_to_effect = HashMap::with_capacity(manager.trigger_speffect_to_buddy_map.len());
    for (effect, buddy_param_id) in manager.trigger_speffect_to_buddy_map.iter() {
        if *effect > 0 && *buddy_param_id > 0 {
            buddy_to_effect.entry(*buddy_param_id).or_insert(*effect);
        }
    }

    let repository = unsafe { SoloParamRepository::instance_mut().ok() };
    let mut groups = HashMap::<i32, Vec<AshHudMember>>::new();
    for owner_groups in manager.groups.iter() {
        for member in owner_groups.second.iter() {
            if member.is_remote {
                continue;
            }
            let Some(effect) = buddy_to_effect.get(&member.buddy_param_id).copied() else {
                continue;
            };
            let chr = unsafe { member.chr_ins.as_ref() };
            let data = chr.modules.as_ref().data.as_ref();
            if data.hp <= 0 || chr.chr_flags1c5.death_flag() {
                continue;
            }

            // NPC name, only used when the Ash item has no name: the spirit's
            // own NpcParam, else the one BuddyParam points to.
            let npc_name = |npc_param_id: i32| {
                (npc_param_id > 0)
                    .then(|| repository.as_deref()?.get::<NpcParam>(npc_param_id as u32))
                    .flatten()
                    .map(|npc| npc.name_id())
                    .filter(|id| *id > 0)
            };
            let npc_name_id = npc_name(chr.npc_param_id)
                .or_else(|| {
                    let buddy = repository
                        .as_deref()?
                        .get::<BuddyParam>(u32::try_from(member.buddy_param_id).ok()?)?;
                    npc_name(buddy.npc_param_id())
                })
                .unwrap_or(0);

            groups.entry(effect).or_default().push(AshHudMember {
                npc_name_id,
                hp: data.hp.max(0) as u32,
                max_hp: data.max_hp.max(0) as u32,
                max_uncapped_hp: data.max_uncapped_hp.max(data.max_hp).max(0) as u32,
                dismissing: member.disappear_requested,
            });
        }
    }
    Some(groups)
}

fn update_status(state: &mut HudState, status: String) {
    if state.last_status != status {
        super::log_line(format_args!("spirit ash HUD: {status}"));
        state.last_status = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ash(name: &str, spirits: usize, hp: u32, dismissing: bool) -> AshSummary {
        AshSummary {
            effect: 0,
            name: name.to_owned(),
            spirits,
            hp,
            max_hp: hp * 2,
            max_uncapped_hp: hp * 2,
            dismissing,
        }
    }

    #[test]
    fn one_row_per_ash_with_group_count() {
        let rows = plan_rows(&[ash("Wolf", 3, 100, false), ash("Jar", 1, 50, true)], 6);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Wolf ×3");
        assert_eq!((rows[0].hp, rows[0].max_hp), (100, 200));
        assert!(!rows[0].dimmed);
        assert_eq!(rows[1].name, "Jar");
        assert!(rows[1].dimmed);
    }

    #[test]
    fn the_last_row_stands_for_the_ashes_that_do_not_fit() {
        let ashes = (0..8)
            .map(|index| ash(&format!("A{index}"), 1, 10, index == 7))
            .collect::<Vec<_>>();
        let rows = plan_rows(&ashes, 6);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[4].name, "A4");
        assert_eq!(rows[5].name, "+3");
        assert_eq!((rows[5].hp, rows[5].max_hp), (30, 60));
        assert!(!rows[5].dimmed);
        assert_eq!(plan_rows(&ashes[..6], 6).len(), 6);
        assert_eq!(plan_rows(&ashes[..6], 6)[5].name, "A5");
        assert_eq!(plan_rows(&ashes, 1)[0].name, "+8");
        assert!(plan_rows(&ashes, 0).is_empty());
    }

    /// Bytes at an RVA of an executable read from disk.
    fn read_rva(exe: &[u8], rva: u32, len: usize) -> &[u8] {
        let word = |at: usize| u16::from_le_bytes([exe[at], exe[at + 1]]) as usize;
        let dword = |at: usize| u32::from_le_bytes(exe[at..at + 4].try_into().unwrap());
        let pe = dword(0x3c) as usize;
        let first = pe + 24 + word(pe + 20);
        let section = (0..word(pe + 6))
            .map(|index| first + index * 40)
            .find(|&entry| (dword(entry + 12)..dword(entry + 12) + dword(entry + 8)).contains(&rva))
            .unwrap();
        let offset = (rva - dword(section + 12) + dword(section + 20)) as usize;
        &exe[offset..offset + len]
    }

    #[test]
    fn native_rvas_match_both_executables() {
        // Each eldenring.exe is read from <MSA_TEST_EXE_DIR or test-gfx\exe>\<folder>.
        let root = std::env::var("MSA_TEST_EXE_DIR")
            .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), r"\test-gfx\exe").to_owned());
        for (label, folder, rvas) in [
            (
                "1.17.1 WW 2.7.1.0",
                "2.7.1.0",
                crate::NATIVE_RVAS_WW2710,
            ),
            (
                "1.17.2 WW 2.7.2.0",
                "2.7.2.0",
                crate::NATIVE_RVAS_WW2720,
            ),
            (
                "1.17.2 JP 2.7.2.1",
                "2.7.2.1-jp",
                crate::NATIVE_RVAS_WW2720,
            ),
        ] {
            let path = format!(r"{root}\{folder}\eldenring.exe");
            let Ok(exe) = std::fs::read(&path) else {
                eprintln!("skipped: {path} not found");
                continue;
            };
            let call = read_rva(&exe, rvas.fe_friend_tag_copy_call_site, 5);
            assert_eq!(call[0], 0xe8, "{label}");
            let target = (rvas.fe_friend_tag_copy_call_site as i64
                + 5
                + i64::from(i32::from_le_bytes(call[1..5].try_into().unwrap())))
                as u32;
            assert_eq!(target, rvas.fe_friend_tag_copy_function, "{label}");
            // mov rax, rsp; push rbp; push rsi; push rdi
            assert_eq!(
                read_rva(&exe, rvas.fe_friend_tag_copy_function, 6),
                [0x48, 0x8b, 0xc4, 0x55, 0x56, 0x57],
                "{label}"
            );
            let lookup = read_rva(&exe, rvas.msg_lookup_function, 10);
            assert_eq!(lookup[..4], MSG_LOOKUP_PROLOGUE, "{label}");
            assert_eq!(lookup[5..10], MSG_LOOKUP_SECOND_CHECK, "{label}");
        }
    }

    #[test]
    fn names_from_messages_are_checked() {
        assert!(valid_name("Lone Wolf Ashes"));
        assert!(valid_name("Cenizas de lobo solitario"));
        assert!(!valid_name("?GoodsName?"));
        assert!(!valid_name(""));
        assert!(!valid_name("204000"));
    }
}
