//! Native Spirit Ash prompt control, ported unchanged from Multiple Spirit
//! Ashes 0.1.165 (`project/src/lib.rs`): GetBuddyState is wrapped at its three
//! menu call sites so state 2 (the replace/additional-summon Yes/No box) and,
//! optionally, state 1 (the standard confirmation) read as 0; the DoSummon
//! gate then lets a recognised new Ash through state 2 or -1 for a short latch.
//! The item-row prompt suppression of 0.1.56 cannot hide this box: the game
//! decides it from GetBuddyState, not from yes_no_dialog_message_id.

use super::*;

pub(crate) const PENDING_SUMMON_GATE_LATCH_MS: u64 = 4_000;

pub(crate) static SUPPRESS_ADDITIONAL_SUMMON_PROMPT_ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) static SUPPRESS_ALL_SPIRIT_ASH_PROMPTS_ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) static ALLOW_LIVING_ASH_DISMISSAL_ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) static NATIVE_SUMMON_PROMPT_QUERY_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_SUMMON_PROMPT_LAST_STATE: AtomicI32 = AtomicI32::new(-1);
pub(crate) static NATIVE_STANDARD_PROMPT_SUPPRESSED_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_SUMMON_PROMPT_SUPPRESSED_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_SUMMON_PROMPT_DISMISSAL_OVERRIDES: AtomicU64 = AtomicU64::new(0);
pub(crate) static ALLOW_PENDING_SUMMON_DO_SUMMON: AtomicBool = AtomicBool::new(false);
pub(crate) static PENDING_SUMMON_GATE_LATCH_DEADLINE_MS: AtomicU64 = AtomicU64::new(0);
pub(crate) static PENDING_SUMMON_GATE_LATCH_GOODS_ID: AtomicIsize = AtomicIsize::new(0);
pub(crate) static NATIVE_SUMMON_PROMPT_HOOKS_READY: AtomicBool = AtomicBool::new(false);
pub(crate) static NATIVE_DO_SUMMON_GATE_READY: AtomicBool = AtomicBool::new(false);
pub(crate) static NATIVE_DO_SUMMON_GATE_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_DO_SUMMON_GATE_ALLOWED_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_DO_SUMMON_GATE_CONTINUED_CALLS: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_DO_SUMMON_GATE_LAST_STATE: AtomicI32 = AtomicI32::new(-1);
pub(crate) static NATIVE_DO_SUMMON_GATE_LAST_PENDING: AtomicBool = AtomicBool::new(false);
pub(crate) static NATIVE_DO_SUMMON_GATE_LAST_ROUTE: AtomicU8 = AtomicU8::new(0);
pub(crate) static NATIVE_DO_SUMMON_STATE2_BYPASSED: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_DO_SUMMON_STATE_MINUS1_BYPASSED: AtomicU64 = AtomicU64::new(0);
pub(crate) static NATIVE_DO_SUMMON_STATE2_FELL_THROUGH: AtomicU64 = AtomicU64::new(0);
pub(crate) static LOG_STARTED: OnceLock<Instant> = OnceLock::new();

pub(crate) fn log_elapsed_ms() -> u64 {
    LOG_STARTED
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(crate) fn arm_pending_summon_gate_latch(goods_id: Option<i32>) {
    let Some(goods_id) = goods_id.filter(|goods_id| *goods_id > 0) else {
        return;
    };
    PENDING_SUMMON_GATE_LATCH_GOODS_ID.store(goods_id as isize, Ordering::Relaxed);
    PENDING_SUMMON_GATE_LATCH_DEADLINE_MS.store(
        log_elapsed_ms().saturating_add(PENDING_SUMMON_GATE_LATCH_MS),
        Ordering::Relaxed,
    );
    ALLOW_PENDING_SUMMON_DO_SUMMON.store(true, Ordering::Release);
}

pub(crate) fn clear_pending_summon_gate_latch() {
    ALLOW_PENDING_SUMMON_DO_SUMMON.store(false, Ordering::Release);
    PENDING_SUMMON_GATE_LATCH_DEADLINE_MS.store(0, Ordering::Relaxed);
    PENDING_SUMMON_GATE_LATCH_GOODS_ID.store(0, Ordering::Relaxed);
}

pub(crate) fn refresh_pending_summon_gate_latch(should_arm: bool, force_clear: bool, goods_id: Option<i32>) {
    if force_clear {
        clear_pending_summon_gate_latch();
        return;
    }
    if should_arm {
        arm_pending_summon_gate_latch(goods_id);
        return;
    }

    let now = log_elapsed_ms();
    let deadline = PENDING_SUMMON_GATE_LATCH_DEADLINE_MS.load(Ordering::Relaxed);
    if deadline > now {
        ALLOW_PENDING_SUMMON_DO_SUMMON.store(true, Ordering::Release);
    } else {
        clear_pending_summon_gate_latch();
    }
}

/// Patch each UI query of GetBuddyState. The wrapper returns the native value
/// except while another Ash is selected, when status 2 would open the vanilla
/// replace-summon confirmation. A live Ash selection keeps its dismissal UI.
pub(crate) fn install_native_summon_prompt_hooks() -> bool {
    let mut installed = 0usize;
    for (index, call_site_rva) in native_rvas().prompt_call_sites.into_iter().enumerate() {
        let success = install_relative_branch_stub(
            call_site_rva,
            native_rvas().buddy_state_function,
            0xe8,
            build_selective_prompt_stub,
            match index {
                0 => "summon prompt UI site 0",
                1 => "summon prompt UI site 1",
                _ => "summon prompt UI site 2",
            },
        );
        installed += usize::from(success);
    }
    log_line(format_args!(
        "native summon-prompt hooks: installed={installed}/{}",
        native_rvas().prompt_call_sites.len()
    ));
    installed == native_rvas().prompt_call_sites.len()
}

/// The final DoSummon gate can receive state -1 before the first summon has
/// initialized its state, and state 2 for a blocked pool summon. Only while a
/// recognized new Ash is pending, a short latch maps either value to the
/// game's allow state 0. Selecting a living Ash clears the latch and preserves
/// the original dismissal flow.
pub(crate) fn install_native_do_summon_gate_hook() -> bool {
    let program = Program::current();
    let Ok(site) = program
        .rva_to_va(native_rvas().do_summon_gate)
        .map(|address| address as usize)
    else {
        log_line("native DoSummon gate: site RVA is outside the executable");
        return false;
    };
    let Ok(allow_target) = program
        .rva_to_va(native_rvas().do_summon_allow)
        .map(|address| address as usize)
    else {
        log_line("native DoSummon gate: allow target is outside the executable");
        return false;
    };
    let Ok(continue_target) = program
        .rva_to_va(native_rvas().do_summon_continue)
        .map(|address| address as usize)
    else {
        log_line("native DoSummon gate: continuation target is outside the executable");
        return false;
    };

    let expected = [0x83, 0xf8, 0x01, 0x76, 0x1b];
    let actual = unsafe { std::slice::from_raw_parts(site as *const u8, expected.len()) };
    if actual != expected || continue_target != site + expected.len() || allow_target != site + 0x20
    {
        log_line(
            "native DoSummon gate: instruction signature or branch target mismatch; no code changed",
        );
        return false;
    }

    let Some(stub) = (unsafe { allocate_near(site) }) else {
        log_line("native DoSummon gate: could not allocate a nearby stub");
        return false;
    };
    let stub_code = build_native_do_summon_gate_stub(allow_target, continue_target);
    unsafe {
        std::ptr::copy_nonoverlapping(stub_code.as_ptr(), stub.cast::<u8>(), stub_code.len());
    }
    if unsafe {
        FlushInstructionCache(
            GetCurrentProcess(),
            Some(stub.cast::<c_void>()),
            stub_code.len(),
        )
    }
    .is_err()
    {
        log_line("native DoSummon gate: stub instruction-cache flush failed");
        return false;
    }

    let relative = (stub as isize).wrapping_sub((site + 5) as isize);
    let Ok(relative) = i32::try_from(relative) else {
        log_line("native DoSummon gate: stub is outside rel32 range");
        return false;
    };
    let mut patch = [0xe9, 0, 0, 0, 0];
    patch[1..].copy_from_slice(&relative.to_le_bytes());
    if !write_executable_patch(site, &patch, "native DoSummon gate") {
        return false;
    }

    log_line(format_args!(
        "native DoSummon gate installed: site_rva={:#x} allow_rva={:#x} continue_rva={:#x} stub={:#x}",
        native_rvas().do_summon_gate,
        native_rvas().do_summon_allow,
        native_rvas().do_summon_continue,
        stub as usize
    ));
    true
}

pub(crate) fn append_absolute_indirect_jump(code: &mut Vec<u8>, target: usize) {
    code.extend_from_slice(&[0xff, 0x25, 0x00, 0x00, 0x00, 0x00]); // jmp qword ptr [rip]
    code.extend_from_slice(&(target as u64).to_le_bytes());
}

pub(crate) fn build_native_do_summon_gate_stub(allow_target: usize, continue_target: usize) -> Vec<u8> {
    let allow_pending = std::ptr::addr_of!(ALLOW_PENDING_SUMMON_DO_SUMMON) as u64;
    let calls = std::ptr::addr_of!(NATIVE_DO_SUMMON_GATE_CALLS) as u64;
    let allowed_calls = std::ptr::addr_of!(NATIVE_DO_SUMMON_GATE_ALLOWED_CALLS) as u64;
    let continued_calls = std::ptr::addr_of!(NATIVE_DO_SUMMON_GATE_CONTINUED_CALLS) as u64;
    let last_state = std::ptr::addr_of!(NATIVE_DO_SUMMON_GATE_LAST_STATE) as u64;
    let last_pending = std::ptr::addr_of!(NATIVE_DO_SUMMON_GATE_LAST_PENDING) as u64;
    let last_route = std::ptr::addr_of!(NATIVE_DO_SUMMON_GATE_LAST_ROUTE) as u64;
    let bypassed = std::ptr::addr_of!(NATIVE_DO_SUMMON_STATE2_BYPASSED) as u64;
    let invalid_state_bypassed = std::ptr::addr_of!(NATIVE_DO_SUMMON_STATE_MINUS1_BYPASSED) as u64;
    let fell_through = std::ptr::addr_of!(NATIVE_DO_SUMMON_STATE2_FELL_THROUGH) as u64;
    let mut code = Vec::with_capacity(256);
    code.extend_from_slice(&[0x41, 0x53]); // push r11; preserve the interrupted function's scratch register
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, gate-call counter
    code.extend_from_slice(&calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, last gate state
    code.extend_from_slice(&last_state.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x89, 0x03]); // mov dword ptr [r11], eax
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, allow-pending-summon flag
    code.extend_from_slice(&allow_pending.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x80, 0x3b, 0x00]); // cmp byte ptr [r11], 0
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, last pending-flag snapshot
    code.extend_from_slice(&last_pending.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x0f, 0x95, 0x03]); // setne byte ptr [r11]; flags stay unchanged
    let je_original = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // je original state handling
    code.extend_from_slice(&[0x83, 0xf8, 0x02]); // cmp eax, 2
    let jne_pending_minus_one = code.len();
    code.extend_from_slice(&[0x75, 0x00]); // if not state 2, check uninitialized state -1
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, state-2 bypass counter
    code.extend_from_slice(&bypassed.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    let jmp_pending_allow_from_state2 = code.len();
    code.extend_from_slice(&[0xeb, 0x00]); // state 2 and -1 share the pending allow route

    let pending_minus_one_check = code.len();
    code[jne_pending_minus_one + 1] = (pending_minus_one_check - (jne_pending_minus_one + 2)) as u8;
    code.extend_from_slice(&[0x83, 0xf8, 0xff]); // cmp eax, -1
    let jne_original = code.len();
    code.extend_from_slice(&[0x75, 0x00]); // other native states keep the original handling
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, state -1 bypass counter
    code.extend_from_slice(&invalid_state_bypassed.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]

    let pending_allow = code.len();
    code[jmp_pending_allow_from_state2 + 1] =
        (pending_allow - (jmp_pending_allow_from_state2 + 2)) as u8;
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, allowed-route counter
    code.extend_from_slice(&allowed_calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, last route
    code.extend_from_slice(&last_route.to_le_bytes());
    code.extend_from_slice(&[0x41, 0xc6, 0x03, 0x01]); // route 1: pending state 2 or -1 mapped to 0
    code.extend_from_slice(&[0x41, 0x5b]); // pop r11
    code.extend_from_slice(&[0x31, 0xc0]); // xor eax, eax: allow this new Ash
    append_absolute_indirect_jump(&mut code, allow_target);

    let original_state_handling = code.len();
    code[je_original + 1] = (original_state_handling - (je_original + 2)) as u8;
    code[jne_original + 1] = (original_state_handling - (jne_original + 2)) as u8;
    code.extend_from_slice(&[0x83, 0xf8, 0x02]); // observe state 2 left to the game's original gate
    let jne_original_compare = code.len();
    code.extend_from_slice(&[0x75, 0x00]); // jne original state comparison
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, state-2 fall-through counter
    code.extend_from_slice(&fell_through.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    let original_compare = code.len();
    code[jne_original_compare + 1] = (original_compare - (jne_original_compare + 2)) as u8;
    code.extend_from_slice(&[0x83, 0xf8, 0x01]); // original cmp eax, 1
    let continue_route = code.len();
    code.extend_from_slice(&[0x77, 0x00]); // unsigned state > 1 continues through the game's gate
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, allowed-route counter
    code.extend_from_slice(&allowed_calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, last route
    code.extend_from_slice(&last_route.to_le_bytes());
    code.extend_from_slice(&[0x41, 0xc6, 0x03, 0x02]); // route 2: original state <= 1 allowed
    code.extend_from_slice(&[0x41, 0x5b]); // pop r11
    append_absolute_indirect_jump(&mut code, allow_target);

    let continue_route_target = code.len();
    code[continue_route + 1] = (continue_route_target - (continue_route + 2)) as u8;
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, continued-route counter
    code.extend_from_slice(&continued_calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, last route
    code.extend_from_slice(&last_route.to_le_bytes());
    code.extend_from_slice(&[0x41, 0xc6, 0x03, 0x03]); // route 3: original state > 1 continued
    code.extend_from_slice(&[0x41, 0x5b]); // pop r11
    append_absolute_indirect_jump(&mut code, continue_target);
    code
}

pub(crate) fn install_relative_branch_stub(
    site_rva: u32,
    expected_target_rva: u32,
    branch_opcode: u8,
    build_stub: fn(usize) -> Vec<u8>,
    label: &str,
) -> bool {
    let program = Program::current();
    let Ok(site) = program.rva_to_va(site_rva).map(|address| address as usize) else {
        log_line(format_args!("{label}: site RVA is outside the executable"));
        return false;
    };
    let Ok(expected_target) = program
        .rva_to_va(expected_target_rva)
        .map(|address| address as usize)
    else {
        log_line(format_args!(
            "{label}: original target RVA is outside the executable"
        ));
        return false;
    };

    let actual = unsafe { std::slice::from_raw_parts(site as *const u8, 5) };
    if actual[0] != branch_opcode {
        log_line(format_args!(
            "{label}: branch opcode mismatch at RVA {site_rva:#x}; no code changed"
        ));
        return false;
    }
    let displacement = i32::from_le_bytes([actual[1], actual[2], actual[3], actual[4]]);
    let encoded_target = ((site + 5) as isize + displacement as isize) as usize;
    if encoded_target != expected_target {
        log_line(format_args!(
            "{label}: original branch target mismatch at RVA {site_rva:#x}; no code changed"
        ));
        return false;
    }

    let Some(stub) = (unsafe { allocate_near(site) }) else {
        log_line(format_args!("{label}: could not allocate a nearby stub"));
        return false;
    };
    let stub_code = build_stub(expected_target);
    unsafe {
        std::ptr::copy_nonoverlapping(stub_code.as_ptr(), stub.cast::<u8>(), stub_code.len());
    }
    if unsafe {
        FlushInstructionCache(
            GetCurrentProcess(),
            Some(stub.cast::<c_void>()),
            stub_code.len(),
        )
    }
    .is_err()
    {
        log_line(format_args!("{label}: stub instruction-cache flush failed"));
        return false;
    }

    let relative = (stub as isize).wrapping_sub((site + 5) as isize);
    let Ok(relative) = i32::try_from(relative) else {
        log_line(format_args!("{label}: stub is outside rel32 range"));
        return false;
    };
    let mut patch = [branch_opcode, 0, 0, 0, 0];
    patch[1..].copy_from_slice(&relative.to_le_bytes());
    if !write_executable_patch(site, &patch, label) {
        return false;
    }

    log_line(format_args!(
        "{label} installed: site_rva={site_rva:#x} original_rva={expected_target_rva:#x} stub={:#x}",
        stub as usize
    ));
    true
}

pub(crate) fn build_selective_prompt_stub(original_target: usize) -> Vec<u8> {
    let suppress_additional = std::ptr::addr_of!(SUPPRESS_ADDITIONAL_SUMMON_PROMPT_ACTIVE) as u64;
    let suppress_all = std::ptr::addr_of!(SUPPRESS_ALL_SPIRIT_ASH_PROMPTS_ACTIVE) as u64;
    let allow_living_dismissal = std::ptr::addr_of!(ALLOW_LIVING_ASH_DISMISSAL_ACTIVE) as u64;
    let query_calls = std::ptr::addr_of!(NATIVE_SUMMON_PROMPT_QUERY_CALLS) as u64;
    let last_state = std::ptr::addr_of!(NATIVE_SUMMON_PROMPT_LAST_STATE) as u64;
    let standard_suppressed_calls =
        std::ptr::addr_of!(NATIVE_STANDARD_PROMPT_SUPPRESSED_CALLS) as u64;
    let suppressed_calls = std::ptr::addr_of!(NATIVE_SUMMON_PROMPT_SUPPRESSED_CALLS) as u64;
    let dismissal_overrides = std::ptr::addr_of!(NATIVE_SUMMON_PROMPT_DISMISSAL_OVERRIDES) as u64;
    let mut code = Vec::with_capacity(224);
    code.extend_from_slice(&[0x48, 0x83, 0xec, 0x28]); // reserve shadow space and align
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, original_target
    code.extend_from_slice(&(original_target as u64).to_le_bytes());
    code.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11
    code.extend_from_slice(&[0x48, 0x83, 0xc4, 0x28]);

    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, prompt-query call counter
    code.extend_from_slice(&query_calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, last native prompt state
    code.extend_from_slice(&last_state.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x89, 0x03]); // mov dword ptr [r11], eax

    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, force-all-confirmations flag
    code.extend_from_slice(&suppress_all.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x80, 0x3b, 0x00]); // cmp byte ptr [r11], 0
    let check_additional_prompt = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // global suppression is off: check additional prompt
    code.extend_from_slice(&[0x83, 0xf8, 0x01]); // cmp eax, 1 (standard summon confirmation)
    let not_standard_confirmation = code.len();
    code.extend_from_slice(&[0x75, 0x00]); // other state: check global replace confirmation
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, standard-confirmation counter
    code.extend_from_slice(&standard_suppressed_calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x31, 0xc0, 0xc3]); // return state 0

    // A new or already-living Ash can use state 2 for its native prompt.
    let check_global_replace_prompt = code.len();
    code[not_standard_confirmation + 1] =
        (check_global_replace_prompt - (not_standard_confirmation + 2)) as u8;
    code.extend_from_slice(&[0x83, 0xf8, 0x02]); // cmp eax, 2
    let global_state_not_suppressed = code.len();
    code.extend_from_slice(&[0x75, 0x00]); // other state: check additional-only policy
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, suppressed-prompt counter
    code.extend_from_slice(&suppressed_calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x31, 0xc0, 0xc3]); // return state 0

    let additional_prompt_check = code.len();
    code[check_additional_prompt + 1] =
        (additional_prompt_check - (check_additional_prompt + 2)) as u8;
    code[global_state_not_suppressed + 1] =
        (additional_prompt_check - (global_state_not_suppressed + 2)) as u8;
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, additional-summon prompt flag
    code.extend_from_slice(&suppress_additional.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x80, 0x3b, 0x00]); // cmp byte ptr [r11], 0
    let additional_prompt_disabled = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // not suppressing additional prompts: check dismissal
    code.extend_from_slice(&[0x83, 0xf8, 0x02]); // cmp eax, 2
    let not_additional_prompt = code.len();
    code.extend_from_slice(&[0x75, 0x00]); // another state: check dismissal eligibility
    // Keep the additional-only setting scoped to state 2.
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, suppressed-prompt counter
    code.extend_from_slice(&suppressed_calls.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x31, 0xc0, 0xc3]); // return state 0

    let dismissal_check = code.len();
    code[additional_prompt_disabled + 1] =
        (dismissal_check - (additional_prompt_disabled + 2)) as u8;
    code[not_additional_prompt + 1] = (dismissal_check - (not_additional_prompt + 2)) as u8;
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, selected-living-Ash flag
    code.extend_from_slice(&allow_living_dismissal.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x80, 0x3b, 0x00]); // cmp byte ptr [r11], 0
    let skip_dismissal_override = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // disabled: keep native eligibility
    let not_blocked = code.len();
    code.extend_from_slice(&[0x83, 0xf8, 0xff, 0x75, 0x00]); // only override native state -1
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, dismissal-eligibility counter
    code.extend_from_slice(&dismissal_overrides.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x31, 0xc0]); // selected Ash is already alive: make dismissal usable
    let native_return = code.len();
    code.push(0xc3); // ret
    code[skip_dismissal_override + 1] = (native_return - (skip_dismissal_override + 2)) as u8;
    code[not_blocked + 4] = (native_return - (not_blocked + 5)) as u8;
    code
}

pub(crate) fn write_executable_patch(address: usize, patch: &[u8], label: &str) -> bool {
    let mut old_protection = PAGE_PROTECTION_FLAGS(0);
    if unsafe {
        VirtualProtect(
            address as *const c_void,
            patch.len(),
            PAGE_EXECUTE_READWRITE,
            &mut old_protection,
        )
    }
    .is_err()
    {
        log_line(format_args!(
            "{label}: could not make the patch site writable"
        ));
        return false;
    }

    unsafe {
        std::ptr::copy_nonoverlapping(patch.as_ptr(), address as *mut u8, patch.len());
    }
    let flushed = unsafe {
        FlushInstructionCache(
            GetCurrentProcess(),
            Some(address as *const c_void),
            patch.len(),
        )
    }
    .is_ok();
    let mut ignored_protection = PAGE_PROTECTION_FLAGS(0);
    let restored = unsafe {
        VirtualProtect(
            address as *const c_void,
            patch.len(),
            old_protection,
            &mut ignored_protection,
        )
    }
    .is_ok();
    if !restored {
        log_line(format_args!(
            "{label}: warning, executable page protection was not restored"
        ));
    }
    if !flushed {
        log_line(format_args!(
            "{label}: instruction-cache flush failed after patch"
        ));
        return false;
    }
    true
}

pub(crate) fn selected_summon_goods_id(state: &RuntimeState) -> Option<i32> {
    let menu = unsafe { CSMenuManImp::instance_mut().ok()? };

    // CSMenuManImp keeps its CSMenuData pointer directly after the vtable.
    // This build-specific read exposes the quick-slot item before the native
    // item-use task queues the confirmation box.
    let menu_data_address = unsafe {
        (menu as *mut CSMenuManImp)
            .cast::<usize>()
            .add(1)
            .read_unaligned()
    };
    if menu_data_address != 0 {
        let menu_data = unsafe { (menu_data_address as *const CSMenuData).as_ref() };
        if let Some(menu_data) = menu_data {
            let raw_item_id = menu_data.menu_gaitem_use_state.quick_slot_item_id;
            let candidates = [raw_item_id, raw_item_id & 0x0fff_ffff];
            for candidate in candidates {
                if let Ok(goods_id) = i32::try_from(candidate)
                    && state.summon_dialog_originals.contains_key(&goods_id)
                {
                    return Some(goods_id);
                }
            }
        }
    }

    // Inventory use can leave the quick-slot field empty. In that case use
    // MenuMan's typed selected-goods item, but only for known Spirit Ash rows.
    let selected_item = menu.player_menu_ctrl.selected_goods_item.as_valid()?;
    if selected_item.category() != ItemCategory::Goods {
        return None;
    }
    let goods_id = i32::try_from(selected_item.param_id()).ok()?;
    state
        .summon_dialog_originals
        .contains_key(&goods_id)
        .then_some(goods_id)
}

/// Per-frame policy for the native prompt and DoSummon hooks, the part of
/// 0.1.165's `update_summon_dialog_policy` that drives them (without its
/// SummonAnywhere and low-FP dismissal branches, which are not built in).
#[allow(clippy::too_many_arguments)]
pub(crate) fn update_native_prompt_flags(
    state: &RuntimeState,
    live_effects: &HashSet<i32>,
    active_goods: i32,
    requested_goods: i32,
    active_goods_ids: &HashSet<i32>,
    suppress_new_summon: bool,
    suppress_all: bool,
    requested_new_summon_goods: Option<i32>,
) {
    // A Spirit Ash's goods ID is also its summon SpEffect ID (228000/228000…),
    // so a live effect identifies the goods too. The goods map alone misses
    // an Ash summoned while no other was alive.
    let goods_is_living =
        |goods_id: i32| active_goods_ids.contains(&goods_id) || live_effects.contains(&goods_id);
    let requested_goods_is_living = goods_is_living(requested_goods);
    // The 0.1.56 policy decides "new" from its goods map only, so it can call
    // a living Ash's goods new (the first Ash summoned): re-check here.
    let requested_new_summon_goods =
        requested_new_summon_goods.filter(|goods_id| !goods_is_living(*goods_id));
    let requested_goods_is_summon = state.summon_dialog_originals.contains_key(&requested_goods);
    let selected_goods = selected_summon_goods_id(state);
    let selected_goods_is_living = selected_goods.is_some_and(goods_is_living);
    let selected_goods_is_summon = selected_goods
        .is_some_and(|goods_id| state.summon_dialog_originals.contains_key(&goods_id));
    // NativeTweaks: hiding state 2 while a living Ash is selected makes the
    // game summon that Ash again, which clears every group, unless the early
    // item task dismisses it first (dismiss with its item + no prompts): then
    // the hooks stay on. At the cap only a new Ash is a problem (the clear
    // guard stands down and the game replaces them all), so the hooks step
    // aside only when a known new Ash is selected or requested there.
    let cap = settings().max_active_ashes;
    let at_cap = cap > 0 && live_effects.len() >= cap;
    let direct_dismissal = settings().toggle_dismiss && suppress_all;
    let new_ash_at_cap = at_cap
        && ((selected_goods_is_summon && !selected_goods_is_living)
            || (selected_goods.is_none() && requested_new_summon_goods.is_some()));
    let step_aside = (selected_goods_is_living && !direct_dismissal) || new_ash_at_cap;
    log_prompt_policy_change(selected_goods, selected_goods_is_living, at_cap, step_aside);
    if step_aside {
        SUPPRESS_ALL_SPIRIT_ASH_PROMPTS_ACTIVE.store(false, Ordering::Release);
        SUPPRESS_ADDITIONAL_SUMMON_PROMPT_ACTIVE.store(false, Ordering::Release);
        ALLOW_LIVING_ASH_DISMISSAL_ACTIVE.store(false, Ordering::Release);
        clear_pending_summon_gate_latch();
        update_param_costs(state, live_effects, active_goods_ids);
        return;
    }
    SUPPRESS_ALL_SPIRIT_ASH_PROMPTS_ACTIVE.store(suppress_all, Ordering::Release);
    let selected_new_summon_goods = (suppress_new_summon
        && !live_effects.is_empty()
        && selected_goods.is_some()
        && !selected_goods_is_living)
        .then_some(selected_goods.unwrap_or_default());
    // A known Ash selected in the quick slot is authoritative: the policy runs
    // ahead of item use, before the engine queues the Yes/No box.
    let pending_new_summon_goods = if selected_goods.is_some() {
        selected_new_summon_goods
    } else {
        requested_new_summon_goods
    };
    let pending_new_summon = pending_new_summon_goods.is_some();
    let requested_goods_is_pending_summon = requested_goods > 0
        && requested_goods != active_goods
        && requested_goods_is_summon
        && !requested_goods_is_living;
    let pending_gate_goods = if selected_goods_is_summon && !selected_goods_is_living {
        selected_goods
    } else if requested_goods_is_pending_summon {
        Some(requested_goods)
    } else {
        pending_new_summon_goods
    };
    let latched_gate_goods = PENDING_SUMMON_GATE_LATCH_GOODS_ID.load(Ordering::Relaxed) as i32;
    // Only clear the latch once the latched new Ash is the requested one and
    // it is living.
    let pending_request_committed = requested_goods_is_living
        && requested_goods > 0
        && requested_goods == latched_gate_goods
        && !pending_new_summon;
    refresh_pending_summon_gate_latch(
        pending_new_summon,
        selected_goods_is_living || pending_request_committed,
        pending_gate_goods,
    );
    ALLOW_LIVING_ASH_DISMISSAL_ACTIVE.store(false, Ordering::Release);
    // Keep the status hook armed whenever an Ash is alive: the selection and
    // request fields can lag a frame behind a new Ash, and letting state 2
    // through then shows the Yes/No box.
    let suppress_native_prompt = suppress_all || (suppress_new_summon && !live_effects.is_empty());
    SUPPRESS_ADDITIONAL_SUMMON_PROMPT_ACTIVE.store(suppress_native_prompt, Ordering::Release);
    update_param_costs(state, live_effects, active_goods_ids);
}

/// Item costs, after the latch is refreshed for this frame. Extra cost first,
/// so the low-FP pass never takes a scaled cost as the original.
fn update_param_costs(
    state: &RuntimeState,
    live_effects: &HashSet<i32>,
    active_goods_ids: &HashSet<i32>,
) {
    super::extras::update_extra_ash_cost(live_effects, active_goods_ids);
    update_living_ash_low_fp_costs(state, live_effects, active_goods_ids);
}

/// Feature switched off: every hook returns the game's own values again.
pub(crate) fn reset_native_prompt_flags() {
    restore_living_ash_low_fp_costs();
    SUPPRESS_ADDITIONAL_SUMMON_PROMPT_ACTIVE.store(false, Ordering::Release);
    SUPPRESS_ALL_SPIRIT_ASH_PROMPTS_ACTIVE.store(false, Ordering::Release);
    ALLOW_LIVING_ASH_DISMISSAL_ACTIVE.store(false, Ordering::Release);
    clear_pending_summon_gate_latch();
}

static LAST_PROMPT_POLICY: Mutex<String> = Mutex::new(String::new());

/// Logs the prompt policy and what the hooked menu sites last saw, whenever
/// either changes (the stubs only count; they cannot log).
fn log_prompt_policy_change(
    selected_goods: Option<i32>,
    selected_goods_is_living: bool,
    at_cap: bool,
    step_aside: bool,
) {
    if !settings().log_enabled {
        return;
    }
    // The counters grow every frame, so only the states decide a new line.
    let line = format!(
        "selected_goods={} selected_living={} at_cap={} step_aside={} menu_last_state={} gate_last_state={} gate_route={}",
        selected_goods.unwrap_or(-1),
        selected_goods_is_living,
        at_cap,
        step_aside,
        NATIVE_SUMMON_PROMPT_LAST_STATE.load(Ordering::Relaxed),
        NATIVE_DO_SUMMON_GATE_LAST_STATE.load(Ordering::Relaxed),
        NATIVE_DO_SUMMON_GATE_LAST_ROUTE.load(Ordering::Relaxed),
    );
    let Ok(mut last) = LAST_PROMPT_POLICY.lock() else {
        return;
    };
    if *last != line {
        log_line(format_args!(
            "native prompt policy: {line} menu_queries={} replace_hidden={} standard_hidden={} cost_restored={}",
            NATIVE_SUMMON_PROMPT_QUERY_CALLS.load(Ordering::Relaxed),
            NATIVE_SUMMON_PROMPT_SUPPRESSED_CALLS.load(Ordering::Relaxed),
            NATIVE_STANDARD_PROMPT_SUPPRESSED_CALLS.load(Ordering::Relaxed),
            NATIVE_SUMMON_COST_RESTORED_CALLS.load(Ordering::Relaxed),
        ));
        *last = line;
    }
}

pub(crate) static NATIVE_SUMMON_COST_RESTORED_CALLS: AtomicU64 = AtomicU64::new(0);

/// NativeTweaks: with an Ash alive GetBuddyState answers 2 ("dismiss the active
/// Ash"), and the item code then zeroes the FP/HP cost and the consumption of
/// the Spirit Ash item, so every additional Ash was free. While the DoSummon
/// latch says a new Ash is pending, these four call sites read 0 instead and
/// the game charges the normal cost (and checks that the player has the FP).
/// Dismissals keep the free state 2: the latch is never armed for a living Ash.
pub(crate) fn install_native_summon_cost_hooks() -> bool {
    let mut installed = 0usize;
    for (index, call_site_rva) in native_rvas().cost_call_sites.into_iter().enumerate() {
        let success = install_relative_branch_stub(
            call_site_rva,
            native_rvas().buddy_state_function,
            0xe8,
            build_summon_cost_stub,
            match index {
                0 => "summon cost site 0 (FP requirement)",
                1 => "summon cost site 1 (HP requirement)",
                2 => "summon cost site 2 (consumption)",
                _ => "summon cost site 3 (consumption)",
            },
        );
        installed += usize::from(success);
    }
    log_line(format_args!(
        "native summon-cost hooks: installed={installed}/{}",
        native_rvas().cost_call_sites.len()
    ));
    installed == native_rvas().cost_call_sites.len()
}

pub(crate) fn build_summon_cost_stub(original_target: usize) -> Vec<u8> {
    let pending = std::ptr::addr_of!(ALLOW_PENDING_SUMMON_DO_SUMMON) as u64;
    let restored = std::ptr::addr_of!(NATIVE_SUMMON_COST_RESTORED_CALLS) as u64;
    let mut code = Vec::with_capacity(64);
    code.extend_from_slice(&[0x48, 0x83, 0xec, 0x28]); // reserve shadow space and align
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, original_target
    code.extend_from_slice(&(original_target as u64).to_le_bytes());
    code.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11
    code.extend_from_slice(&[0x48, 0x83, 0xc4, 0x28]);
    code.extend_from_slice(&[0x83, 0xf8, 0x02]); // cmp eax, 2
    let not_state_two = code.len();
    code.extend_from_slice(&[0x75, 0x00]); // other states keep their native cost
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, pending-new-Ash flag
    code.extend_from_slice(&pending.to_le_bytes());
    code.extend_from_slice(&[0x41, 0x80, 0x3b, 0x00]); // cmp byte ptr [r11], 0
    let no_pending_summon = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // dismissal or no new Ash: keep state 2 (free)
    code.extend_from_slice(&[0x49, 0xbb]); // mov r11, restored-cost counter
    code.extend_from_slice(&restored.to_le_bytes());
    code.extend_from_slice(&[0xf0, 0x49, 0xff, 0x03]); // lock inc qword ptr [r11]
    code.extend_from_slice(&[0x31, 0xc0]); // state 0: charge the normal cost
    let done = code.len();
    code.push(0xc3); // ret
    code[not_state_two + 1] = (done - (not_state_two + 2)) as u8;
    code[no_pending_summon + 1] = (done - (no_pending_summon + 2)) as u8;
    code
}

/// Goods of living Ashes whose consumeMP was set to 0, with the value it had.
static LOW_FP_COST_OVERRIDES: LazyLock<Mutex<HashMap<i32, i16>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Like AllowLivingAshDismissalWhenBlocked in 0.1.165: the game treats only the
/// Ash in its single active slot as summoned, so with too little FP every other
/// living Ash's item is greyed out and cannot be used to dismiss it. While the
/// player's FP is below such an item's cost, its consumeMP is 0; back to its
/// value once the Ash is gone or the FP is enough. New summons keep their cost.
/// The value is taken when it is zeroed and written back only if it is still
/// 0, so a change from the Hub's Spirit ash cost (%) slider is never undone.
fn update_living_ash_low_fp_costs(
    state: &RuntimeState,
    live_effects: &HashSet<i32>,
    active_goods_ids: &HashSet<i32>,
) {
    let Ok(mut overrides) = LOW_FP_COST_OVERRIDES.lock() else {
        return;
    };
    if overrides.is_empty() && live_effects.is_empty() {
        return;
    }
    let player_fp = unsafe { WorldChrMan::instance() }
        .ok()
        .and_then(|world| world.main_player.as_deref())
        .map(|player| player.chr_ins.modules.data.fp);
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let living = active_goods_ids
        .iter()
        .chain(live_effects.iter())
        .copied()
        .filter(|goods_id| state.summon_dialog_originals.contains_key(goods_id))
        .collect::<HashSet<_>>();
    let mut desired = HashSet::new();
    if settings().toggle_dismiss
        && settings().low_fp_dismissal
        && let Some(fp) = player_fp
    {
        for goods_id in &living {
            let cost = overrides.get(goods_id).copied().or_else(|| {
                repository
                    .get::<EquipParamGoods>(*goods_id as u32)
                    .map(|row| row.consume_mp())
            });
            if cost.is_some_and(|cost| cost > 0 && i32::from(cost) > fp) {
                desired.insert(*goods_id);
            }
        }
    }

    let mut restored = Vec::new();
    overrides.retain(|goods_id, original| {
        if desired.contains(goods_id) {
            return true;
        }
        if let Some(row) = repository.get_mut::<EquipParamGoods>(*goods_id as u32)
            && row.consume_mp() == 0
        {
            row.set_consume_mp(*original);
        }
        restored.push((*goods_id, *original));
        false
    });
    let mut zeroed = Vec::new();
    for goods_id in desired {
        let Some(row) = repository.get_mut::<EquipParamGoods>(goods_id as u32) else {
            continue;
        };
        let current = row.consume_mp();
        if current == 0 {
            continue;
        }
        overrides.insert(goods_id, current);
        row.set_consume_mp(0);
        zeroed.push((goods_id, current));
    }
    if !zeroed.is_empty() || !restored.is_empty() {
        log_line(format_args!(
            "living Ash low-FP dismissal: player_fp={player_fp:?} zeroed_goods_costs={zeroed:?} restored_goods_costs={restored:?}"
        ));
    }
}

/// Feature switched off: every consumeMP set to 0 above gets its value back.
fn restore_living_ash_low_fp_costs() {
    let Ok(mut overrides) = LOW_FP_COST_OVERRIDES.lock() else {
        return;
    };
    if overrides.is_empty() {
        return;
    }
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    for (goods_id, original) in overrides.drain() {
        if let Some(row) = repository.get_mut::<EquipParamGoods>(goods_id as u32)
            && row.consume_mp() == 0
        {
            row.set_consume_mp(original);
        }
    }
}
