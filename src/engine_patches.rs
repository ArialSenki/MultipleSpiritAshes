//! Game patches that NativeTweaks applies from its Hub (src/vanilla_settings.cpp,
//! where each one is explained) and the standalone DLL applies here, once,
//! from its INI:
//! - up to 500 spirits: the summon_buddy ChrSet is created with 520 entries
//!   instead of 80, and the slot search runs over all of them from slot 20
//!   instead of the 10 slots of the player's own block;
//! - Summon anywhere: the summoning-area state check, the item's area check and
//!   "within activation range" all pass.
//! Every site is found by its bytes in the game's code and left alone when it
//! is not found exactly once.

use super::*;

/// The game's first .text section (its code; the second one is Arxan's).
fn game_code() -> Option<(usize, &'static [u8])> {
    let base = unsafe { GetModuleHandleW(PCWSTR::null()) }.ok()?.0 as usize;
    let read_u16 = |at: usize| unsafe { (at as *const u16).read_unaligned() };
    let read_u32 = |at: usize| unsafe { (at as *const u32).read_unaligned() };
    let pe = base + read_u32(base + 0x3c) as usize;
    let sections = read_u16(pe + 6) as usize;
    let first = pe + 24 + read_u16(pe + 20) as usize;
    (0..sections).find_map(|index| {
        let header = first + index * 40;
        let name = unsafe { std::slice::from_raw_parts(header as *const u8, 8) };
        (name == b".text\0\0\0").then(|| {
            let size = read_u32(header + 8) as usize;
            let rva = read_u32(header + 12) as usize;
            let start = base + rva;
            (start, unsafe { std::slice::from_raw_parts(start as *const u8, size) })
        })
    })
}

/// "aa ? bb": the address of the only match, None for none or several.
fn find_unique(code: &(usize, &[u8]), pattern: &str) -> Option<usize> {
    let wanted: Vec<Option<u8>> = pattern
        .split_whitespace()
        .map(|token| (token != "?").then(|| u8::from_str_radix(token, 16).ok()).flatten())
        .collect();
    let first = wanted.first().copied().flatten()?;
    let (start, bytes) = code;
    let mut found = None;
    let last = bytes.len().checked_sub(wanted.len())?;
    for offset in 0..=last {
        if bytes[offset] != first {
            continue;
        }
        let matches = wanted
            .iter()
            .enumerate()
            .all(|(i, byte)| byte.is_none_or(|byte| bytes[offset + i] == byte));
        if matches {
            if found.is_some() {
                return None;
            }
            found = Some(start + offset);
        }
    }
    found
}

/// Writes `bytes` at `pattern`'s match + `offset` when the bytes there are
/// `expected`.
fn patch(code: &(usize, &[u8]), label: &str, pattern: &str, offset: usize, expected: &[u8], bytes: &[u8]) -> bool {
    let Some(hit) = find_unique(code, pattern) else {
        log_line(format_args!("{label}: not found (or ambiguous); left alone"));
        return false;
    };
    let address = hit + offset;
    let current = unsafe { std::slice::from_raw_parts(address as *const u8, expected.len()) };
    if current != expected {
        log_line(format_args!("{label}: unexpected bytes; left alone"));
        return false;
    }
    native_prompt::write_executable_patch(address, bytes, label)
}

const BUDDY_SET_INIT: &str = "ba 00 05 00 00 41 b8 08 00 00 00 ff 50 50 48 89 46 18 48 85 c0 74 ? \
    bf 50 00 00 00 89 7e 10 33 d2 41 b8 00 05 00 00";
const SLOT_START: &str = "0f b6 c2 83 c0 02 8d 04 80 44 8d 14 00 b8 67 66 66 66 41 f7 e9";
const SLOT_LOOP: [u8; 57] = [
    0x41, 0x83, 0xf9, 0x0a, 0x7d, 0x0a, 0x43, 0x8d, 0x3c, 0x11, 0x49, 0x8d, 0x1c, 0x12, 0xeb, 0x0e,
    0x41, 0x8d, 0x7a, 0xf6, 0x41, 0x03, 0xf9, 0x49, 0x8d, 0x5a, 0xf6, 0x48, 0x03, 0xda, 0x48, 0x03,
    0xd9, 0x48, 0xc1, 0xe3, 0x04, 0x49, 0x03, 0xd8, 0x4c, 0x39, 0x3b, 0x74, 0x14, 0x41, 0xff, 0xc1,
    0x48, 0xff, 0xc1, 0x48, 0x83, 0xf9, 0x0a, 0x7c, 0xc7,
];

/// The spirit set's size (it is created once, early: patched as soon as the
/// DLL starts) and the slot search over the whole set.
fn spirit_slots(code: &(usize, &[u8])) {
    let Some(init) = find_unique(code, BUDDY_SET_INIT) else {
        log_line("spirit set: allocation code not found (or ambiguous); left at 80 entries");
        return;
    };
    // 520 entries of 16 bytes, written together once all three were checked.
    let writes: [(usize, &[u8], &[u8]); 3] = [
        (0, &[0xba, 0x00, 0x05, 0x00, 0x00], &[0xba, 0x80, 0x20, 0x00, 0x00]),
        (0x17, &[0xbf, 0x50, 0x00, 0x00, 0x00], &[0xbf, 0x08, 0x02, 0x00, 0x00]),
        (0x21, &[0x41, 0xb8, 0x00, 0x05, 0x00, 0x00], &[0x41, 0xb8, 0x80, 0x20, 0x00, 0x00]),
    ];
    let set_ok = writes.iter().all(|(offset, expected, _)| {
        (unsafe { std::slice::from_raw_parts((init + offset) as *const u8, expected.len()) }) == *expected
    }) && writes
        .iter()
        .all(|(offset, _, bytes)| native_prompt::write_executable_patch(init + offset, bytes, "spirit set"));
    log_line(format_args!(
        "spirit set: created with {} entries",
        if set_ok { 520 } else { 80 }
    ));

    // The search: start at 20 for everyone, then a cave that runs over every
    // slot up to the set's real capacity (r14 = ChrSet, r8 = entries, r9d =
    // hint, r10d = 20, rcx = 0, r15 = 0; a free slot leaves its index in edi
    // and its entry in rbx, as the original loop does).
    let loop_pattern = SLOT_LOOP.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ") + " 49 8b c7";
    let Some(site) = find_unique(code, &loop_pattern) else {
        log_line("spirit slots: search loop not found (or ambiguous); kept at 10 per player");
        return;
    };
    let Some(cave) = (unsafe { allocate_near(site) }).map(|page| page as usize) else {
        log_line("spirit slots: no memory near the game code; kept at 10 per player");
        return;
    };
    let mut body: Vec<u8> = vec![
        0x41, 0x8b, 0x46, 0x10, // mov eax,[r14+10h]   capacity
        0x83, 0xe8, 0x14, // sub eax,20          slots from 20
    ];
    let loop_start = body.len();
    body.extend_from_slice(&[
        0x41, 0x39, 0xc1, // cmp r9d,eax
        0x43, 0x8d, 0x3c, 0x11, // lea edi,[r9+r10]
        0x7c, 0x02, // jl have
        0x29, 0xc7, // sub edi,eax         wrap round
        0x48, 0x63, 0xdf, // have: movsxd rbx,edi
        0x48, 0xc1, 0xe3, 0x04, // shl rbx,4
        0x4c, 0x01, 0xc3, // add rbx,r8
        0x4c, 0x39, 0x3b, // cmp [rbx],r15
        0x74, 0x00, // je found
    ]);
    let to_found = body.len();
    body.extend_from_slice(&[
        0x41, 0xff, 0xc1, // inc r9d
        0x48, 0xff, 0xc1, // inc rcx
        0x48, 0x39, 0xc1, // cmp rcx,rax
        0x7c, 0x00, // jl loop
    ]);
    let back = body.len();
    body[back - 1] = (loop_start as isize - back as isize) as u8;
    body.extend_from_slice(&[0xff, 0x25, 0, 0, 0, 0]); // none free: after the loop
    body.extend_from_slice(&((site + 57) as u64).to_le_bytes());
    body[to_found - 1] = (body.len() - to_found) as u8;
    body.extend_from_slice(&[0xff, 0x25, 0, 0, 0, 0]); // found: past "mov rax,r15; jmp"
    body.extend_from_slice(&((site + 65) as u64).to_le_bytes());
    unsafe { std::ptr::copy_nonoverlapping(body.as_ptr(), cave as *mut u8, body.len()) };

    let mut jump = [0x90u8; 57];
    jump[0] = 0xe9;
    let displacement = (cave as i64 - (site as i64 + 5)) as i32;
    jump[1..5].copy_from_slice(&displacement.to_le_bytes());
    let start_ok = patch(
        code,
        "spirit slots (start)",
        SLOT_START,
        3,
        &[0x83, 0xc0, 0x02, 0x8d, 0x04, 0x80, 0x44, 0x8d, 0x14, 0x00],
        &[0x41, 0xba, 0x14, 0x00, 0x00, 0x00, 0x0f, 0x1f, 0x40, 0x00],
    );
    let search_ok = start_ok && native_prompt::write_executable_patch(site, &jump, "spirit slots (search)");
    log_line(format_args!(
        "spirit slots: {}",
        if search_ok { "shared up to the set's capacity" } else { "10 per player" }
    ));
}

/// Summon anywhere: the three checks that keep summoning to a monument's area.
fn summon_anywhere(code: &(usize, &[u8])) {
    let area = patch(
        code,
        "summoning area",
        "ba 75 01 00 00 48 8b 88 78 01 00 00 e8 ? ? ? ? 84 c0 74 ? 8b 43 38 85 c0 74 ? 3b 43 3c",
        17,
        &[0x84, 0xc0, 0x74],
        &[0x84, 0xc0, 0x90, 0x90],
    );
    let mut item_bytes = vec![0xb0, 0x01, 0x48, 0x83, 0xc4, 0x28, 0xc3];
    item_bytes.resize(21, 0x90);
    let item = patch(
        code,
        "summoning area (items)",
        "48 8b 80 08 e5 01 00 ba 75 01 00 00 48 8b 88 78 01 00 00 48 83 c4 28 e9",
        7,
        &[0xba, 0x75, 0x01, 0x00, 0x00, 0x48, 0x8b, 0x88, 0x78, 0x01, 0x00, 0x00, 0x48, 0x83, 0xc4, 0x28, 0xe9],
        &item_bytes,
    );
    let range = patch(
        code,
        "summoning range",
        "44 38 ae b5 00 00 00 74 07 44 88 ae b7 00 00 00 4c 39 ae 00 01 00 00",
        0,
        &[0x44, 0x38, 0xae, 0xb5, 0x00, 0x00, 0x00, 0x74, 0x07],
        &[0xc6, 0x86, 0xb5, 0x00, 0x00, 0x00, 0x01, 0x66, 0x90],
    );
    log_line(format_args!(
        "summon anywhere: area check {}, item check {}, range {}",
        if area { "ok" } else { "not patched" },
        if item { "ok" } else { "not patched" },
        if range { "ok" } else { "not patched" }
    ));
}

/// Called once by the standalone DLL right after nt_msa_start.
#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_engine_patches(several_ashes: u8, anywhere: u8) {
    let Some(code) = game_code() else {
        log_line("engine patches: the game's code section was not found");
        return;
    };
    if several_ashes != 0 {
        spirit_slots(&code);
    }
    if anywhere != 0 {
        summon_anywhere(&code);
    }
}
