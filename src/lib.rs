mod area_follow;
mod extras;
mod game_menus;
mod hud;
mod hud_movie;
mod native_prompt;
mod perf;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::{OsString, c_void},
    fs::{File, OpenOptions},
    io::Write,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, Once, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use eldenring::{
    cs::{
        BuddyStoneParam, CSMenuData, CSMenuManImp, CSTaskGroupIndex, CSTaskImp, ChrIns, ChrInsExt, EnemyIns,
        EquipParamGoods, ItemCategory,
        SoloParamRepository, SpEffectParam, SpEffectVfxParam, SummonBuddyGroup, SummonBuddyManager,
        WorldChrMan,
    },
    fd4::FD4TaskData,
    position::HavokPosition,
    util::system::wait_for_system_init,
};
use fromsoftware_shared::{
    FromStatic, GameVersion, LANG_ID_EN, LANG_ID_JP, SharedTaskImpExt, Superclass, program::Program,
};
use area_follow::update_area_follow;
pub use extras::{nt_msa_dismiss_all, nt_msa_set_ash_cost_percent, nt_msa_set_buddy_stone_rules};
pub use game_menus::{nt_game_menu_last_result, nt_open_game_menu};
use native_prompt::*;
use pelite::pe64::Pe;
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::HMODULE,
    System::{
        Diagnostics::Debug::FlushInstructionCache,
        LibraryLoader::{GetModuleFileNameW, GetModuleHandleW},
        Memory::{
            MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualAlloc,
            VirtualProtect,
        },
        Threading::GetCurrentProcess,
    },
};

const LOG_FILE_NAME: &str = "NativeTweaks.SpiritAshes.log";
const STANDALONE_LOG_FILE_NAME: &str = "MultipleSpiritAshes.log";
const BUILD_VERSION: &str = "0.2.0-ww2710-ww2720";
/// The standalone mod; it must not run beside the built-in copy.
const STANDALONE_DLL_NAME: &str = "MultipleSpiritAshes.dll";
const MIN_ITEM_ACTION_INTERVAL_SECONDS: f32 = 0.75;
const DISMISS_FADE_SECONDS: f32 = 1.5;
const DISMISS_VISUAL_SP_EFFECT_ID: i32 = 16316;
const DISMISS_VFX_RELEASE_REMAINING_SECONDS: f32 = 0.25;
const NATIVE_EXIT_ANIMATION_ID: i32 = 1840;
const NATIVE_EXIT_HANDOFF_TIMEOUT: Duration = Duration::from_millis(750);
const NATIVE_EXIT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(12);
const NATIVE_EXIT_REMOVE_MARGIN_SECONDS: f32 = 0.15;
/// eldenring.exe RVAs of the native code MSA hooks. Every site is still
/// checked against its byte signature before it is changed.
#[derive(Clone, Copy)]
struct NativeRvas {
    clear_callsite: u32,
    clear_function: u32,
    /// GetBuddyState and its three menu call sites (0.1.165 prompt hooks).
    buddy_state_function: u32,
    prompt_call_sites: [u32; 3],
    /// DoSummon's GetBuddyState gate (`cmp eax, 1; jbe allow`).
    do_summon_gate: u32,
    do_summon_allow: u32,
    do_summon_continue: u32,
    /// GetBuddyState calls that zero the FP/HP cost (CanUseItem) and the
    /// consumption (item use) on state 2. NativeTweaks, not in 0.1.165.
    cost_call_sites: [u32; 4],
    exit_function: u32,
    exit_group_filter: u32,
    /// CSFeManImp's per-frame copy of its friendly tag entries into the HUD
    /// values, and its call in the FeMan update (compact HUD rows).
    fe_friend_tag_copy_call_site: u32,
    fe_friend_tag_copy_function: u32,
    /// MsgRepositoryImp::LookupEntry (Ash names on the HUD rows).
    msg_lookup_function: u32,
}

/// WW 2.7.1.0 (Elden Ring 1.17.1).
const NATIVE_RVAS_WW2710: NativeRvas = NativeRvas {
    clear_callsite: 0x4b89b0,
    clear_function: 0x4bb9a0,
    buddy_state_function: 0x4b72f0,
    prompt_call_sites: [0x7c3ca4, 0x847a20, 0x847ce4],
    do_summon_gate: 0x4b85ce,
    do_summon_allow: 0x4b85ee,
    do_summon_continue: 0x4b85d3,
    cost_call_sites: [0x69018a, 0x69022d, 0x3c0b79, 0x3c0c2f],
    exit_function: 0x4b8160,
    exit_group_filter: 0x4b8224,
    fe_friend_tag_copy_call_site: 0x772b0d,
    fe_friend_tag_copy_function: 0x774fd0,
    msg_lookup_function: 0x266fc40,
};

/// WW 2.7.2.0 (Elden Ring 1.17.2): the same code moved by +0xe0. Verified
/// offline against both executables (research/verify_native_rvas.py).
const NATIVE_RVAS_WW2720: NativeRvas = NativeRvas {
    clear_callsite: 0x4b8a90,
    clear_function: 0x4bba80,
    buddy_state_function: 0x4b73d0,
    prompt_call_sites: [0x7c4154, 0x847ed0, 0x848194],
    do_summon_gate: 0x4b86ae,
    do_summon_allow: 0x4b86ce,
    do_summon_continue: 0x4b86b3,
    cost_call_sites: [0x69063a, 0x6906dd, 0x3c0b79, 0x3c0c2f],
    exit_function: 0x4b8240,
    exit_group_filter: 0x4b8304,
    fe_friend_tag_copy_call_site: 0x772fbd,
    fe_friend_tag_copy_function: 0x775480,
    msg_lookup_function: 0x2671370,
};

static NATIVE_RVAS: OnceLock<NativeRvas> = OnceLock::new();

/// RVAs for the running executable; set before any hook is installed.
fn native_rvas() -> &'static NativeRvas {
    NATIVE_RVAS.get().unwrap_or(&NATIVE_RVAS_WW2710)
}
const NATIVE_EXIT_FUNCTION_PROLOGUE: [u8; 8] = [0x40, 0x55, 0x56, 0x57, 0x48, 0x83, 0xec, 0x30];
const NATIVE_EXIT_GROUP_FILTER_SIGNATURE: [u8; 10] =
    [0x80, 0x7b, 0x31, 0x00, 0x0f, 0x85, 0x84, 0x00, 0x00, 0x00];

static DLL_MODULE: AtomicIsize = AtomicIsize::new(0);
static INSTALL_TASKS: Once = Once::new();
static NULL_MANAGER_REPORTED: AtomicBool = AtomicBool::new(false);
static BUDDY_ITEM_LIMITS_PATCHED: AtomicBool = AtomicBool::new(false);
static PARAM_REPOSITORY_WAIT_REPORTED: AtomicBool = AtomicBool::new(false);
static PARAM_TARGETS_WAIT_REPORTED: AtomicBool = AtomicBool::new(false);
static DISMISS_VFX_PARAM_REPORTED: AtomicBool = AtomicBool::new(false);
static DISMISS_VFX_PARAM_MISSING_REPORTED: AtomicBool = AtomicBool::new(false);
static LAST_CLEARED_BUDDY_STONE_EFFECT: AtomicIsize = AtomicIsize::new(0);
static LAST_ELIGIBILITY_REQUEST_EFFECT: AtomicIsize = AtomicIsize::new(-1);
static NATIVE_CLEAR_GUARD_READY: AtomicBool = AtomicBool::new(false);
static NATIVE_CLEAR_GUARD_STUB: AtomicIsize = AtomicIsize::new(0);
static ORIGINAL_NATIVE_CLEAR_GROUPS: AtomicIsize = AtomicIsize::new(0);
static NATIVE_EXIT_FILTER_READY: AtomicBool = AtomicBool::new(false);
static NATIVE_EXIT_FILTER_PAGE: AtomicIsize = AtomicIsize::new(0);
static ORIGINAL_NATIVE_EXIT_ROUTINE: AtomicIsize = AtomicIsize::new(0);
static NATIVE_EXIT_TARGET_STARTED: AtomicIsize = AtomicIsize::new(-1);
static NATIVE_EXIT_HOOK_CALLS: AtomicU64 = AtomicU64::new(0);
static NATIVE_EXIT_HOOK_CALLS_AT_HANDOFF: AtomicU64 = AtomicU64::new(0);
static NATIVE_EXIT_TRACE_LAST_KEY: AtomicIsize = AtomicIsize::new(-1);
static NATIVE_EXIT_LAST_PASSTHROUGH_KEY: AtomicIsize = AtomicIsize::new(-1);
static NATIVE_EXIT_LAST_SCOPED_LOG_EFFECT: AtomicIsize = AtomicIsize::new(-1);
static PENDING_SWITCH_CLEAR_EFFECT: AtomicIsize = AtomicIsize::new(-1);
static PENDING_SWITCH_GOODS_ID: AtomicIsize = AtomicIsize::new(-1);
static PENDING_ITEM_DISMISS_EFFECT: AtomicIsize = AtomicIsize::new(-1);
static PENDING_ITEM_DISMISS_GOODS: AtomicIsize = AtomicIsize::new(-1);
static LAST_SPAWN_SYNC_EFFECT: AtomicIsize = AtomicIsize::new(-1);
static REPORTED_PRESERVED_EFFECTS: LazyLock<Mutex<HashSet<i32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static SETTINGS: RwLock<Settings> = RwLock::new(Settings::DEFAULT);
static LOG: LazyLock<Mutex<Option<File>>> = LazyLock::new(|| Mutex::new(None));
static STATE: LazyLock<Mutex<RuntimeState>> = LazyLock::new(|| Mutex::new(RuntimeState::default()));
static STARTED: AtomicBool = AtomicBool::new(false);
/// Started from MultipleSpiritAshes.dll itself (standalone/), not NativeTweaks.
static STANDALONE_BUILD: AtomicBool = AtomicBool::new(false);
/// The HUD movie hook went in at startup: the vanilla Ash panel is hidden, so
/// the compact rows keep running even after the feature is switched off.
static HUD_ROWS_INSTALLED: AtomicBool = AtomicBool::new(false);

/// What NativeTweaks controls (layout shared with src/spirit_ashes.cpp).
/// Everything else keeps the defaults of the standalone mod's shipped INI.
#[repr(C)]
pub struct NtMsaSettings {
    pub enabled: u8,
    pub max_active_ashes: u32,
    pub log_enabled: u8,
    /// Using a summoned Ash's item again dismisses only that Ash.
    pub toggle_dismiss: u8,
    /// Hides every Spirit Ash confirmation: the item rows' prompts are cleared
    /// up front, so none can open before the policy sees the new selection.
    pub suppress_prompts: u8,
    /// After a block change or other player jump, raise the native "warp
    /// all" flag so the Ashes follow (area_follow.rs).
    pub follow_areas: u8,
    /// Remove a dismissed Ash after a 1.5 s fade instead of the full native
    /// exit animation (~6.7 s).
    pub fast_dismissal: u8,
    /// Living Ashes' items stay usable for dismissal with too little FP.
    pub low_fp_dismissal: u8,
    /// Compact HUD rows (the HUD movie patch); read at startup only.
    pub hud_rows: u8,
    /// FP cost of each additional Ash, in % of its own (100 = native).
    pub extra_cost_percent: u32,
    /// Damage lost per extra living Ash, in % (0 = native).
    pub damage_reduction_percent: u32,
}

/// Starts the built-in Multiple Spirit Ashes once. `module` is NativeTweaks.dll
/// (the log goes beside it). Returns 1 when the game version is supported and
/// the worker started, 0 otherwise.
///
/// # Safety
/// `hub` must point to a valid `NtMsaSettings` for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nt_msa_start(module: *mut c_void, hub: *const NtMsaSettings) -> u8 {
    let Some(initial) = (unsafe { hub.as_ref() }) else {
        return 0;
    };
    if STARTED.swap(true, Ordering::AcqRel) {
        return 0;
    }
    DLL_MODULE.store(module as isize, Ordering::Relaxed);
    STANDALONE_BUILD.store(module_is_standalone_dll(), Ordering::Release);
    apply_hub_settings(initial);
    let Some(version) = supported_version(Program::current()) else {
        log_line("unsupported executable version; no game memory was changed");
        return 0;
    };
    if standalone_loaded() {
        log_line("MultipleSpiritAshes.dll is loaded: the built-in copy stays off");
        return 0;
    }

    thread::spawn(move || {
        let program = Program::current();
        // The HUD movie is opened early, so this hook goes in before the
        // start delay; it patches 01_000_fe.gfx in memory when it loads.
        // Only when the feature is on at startup: off, the vanilla panel stays.
        // Only where the HUD row hooks exist (WW 2.7.1.0 / 2.7.2.0): elsewhere
        // the patch would hide the vanilla panel with no rows to replace it.
        if settings().enabled && settings().patch_hud_movie && version.native_rvas().is_some() {
            HUD_ROWS_INSTALLED.store(hud_movie::install(), Ordering::Release);
        } else {
            log_line("HUD movie patch skipped (feature off at startup); menu/01_000_fe.gfx served unchanged");
        }
        thread::sleep(settings().start_delay());
        if wait_for_system_init(&program, Duration::MAX).is_err() {
            log_line("game system initialisation did not complete");
            return;
        }
        if standalone_loaded() {
            log_line("MultipleSpiritAshes.dll was loaded later: the built-in copy stays off");
            return;
        }
        install_tasks_once(version);
    });
    1
}

/// Applies changed settings at once (the tasks read them every frame).
///
/// # Safety
/// `hub` must point to a valid `NtMsaSettings` for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nt_msa_set(hub: *const NtMsaSettings) {
    if let Some(changed) = unsafe { hub.as_ref() } {
        apply_hub_settings(changed);
    }
}

fn apply_hub_settings(hub: &NtMsaSettings) {
    if let Ok(mut current) = SETTINGS.write() {
        current.enabled = hub.enabled != 0;
        current.max_active_ashes = hub.max_active_ashes as usize;
        current.log_enabled = hub.log_enabled != 0;
        current.toggle_dismiss = hub.toggle_dismiss != 0;
        current.suppress_spirit_ash_confirmation = hub.suppress_prompts != 0;
        current.suppress_additional_summon_prompt = hub.suppress_prompts != 0;
        current.follow_areas = hub.follow_areas != 0;
        current.fast_dismissal = hub.fast_dismissal != 0;
        current.low_fp_dismissal = hub.low_fp_dismissal != 0;
        current.patch_hud_movie = hub.hud_rows != 0;
        current.extra_cost_percent = hub.extra_cost_percent.min(1000);
        current.damage_reduction_percent = hub.damage_reduction_percent.min(100);
    }
}

/// The module that started this copy is MultipleSpiritAshes.dll.
fn module_is_standalone_dll() -> bool {
    let module = DLL_MODULE.load(Ordering::Relaxed);
    if module == 0 {
        return false;
    }
    let mut buffer = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(HMODULE(module as *mut _)), &mut buffer) };
    if len == 0 || len as usize >= buffer.len() {
        return false;
    }
    PathBuf::from(OsString::from_wide(&buffer[..len as usize]))
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(STANDALONE_DLL_NAME))
}

/// Another copy is loaded as MultipleSpiritAshes.dll: the built-in one stays
/// off (the standalone DLL never counts itself).
fn standalone_loaded() -> bool {
    if STANDALONE_BUILD.load(Ordering::Acquire) {
        return false;
    }
    standalone_dll_present()
}

fn standalone_dll_present() -> bool {
    let name = STANDALONE_DLL_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe { GetModuleHandleW(PCWSTR(name.as_ptr())) }.is_ok()
}

#[derive(Clone, Copy)]
enum SupportedErVersion {
    Ww2700,
    Ww2710,
    Ww2720,
    Jp2701,
    Jp2721,
}

impl GameVersion for SupportedErVersion {
    const NAME: &'static str = "elden ring";

    fn from_lang_version(lang_id: u16, version: &str) -> Option<Self> {
        match (lang_id, version) {
            (LANG_ID_EN, "2.7.0.0") => Some(Self::Ww2700),
            (LANG_ID_EN, "2.7.1.0") => Some(Self::Ww2710),
            (LANG_ID_EN, "2.7.2.0") => Some(Self::Ww2720),
            (LANG_ID_JP, "2.7.0.1") => Some(Self::Jp2701),
            (LANG_ID_JP, "2.7.2.1") => Some(Self::Jp2721),
            _ => None,
        }
    }
}

impl SupportedErVersion {
    /// Executables whose native hook sites were verified.
    fn native_rvas(self) -> Option<NativeRvas> {
        match self {
            Self::Ww2710 => Some(NATIVE_RVAS_WW2710),
            // JP 2.7.2.1 has the code of WW 2.7.2.0 at every address in this
            // table (compared byte for byte, 24-10).
            Self::Ww2720 | Self::Jp2721 => Some(NATIVE_RVAS_WW2720),
            Self::Ww2700 | Self::Jp2701 => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ww2700 => "WW 2.7.0.0",
            Self::Ww2710 => "WW 2.7.1.0",
            Self::Ww2720 => "WW 2.7.2.0",
            Self::Jp2701 => "JP 2.7.0.1",
            Self::Jp2721 => "JP 2.7.2.1",
        }
    }
}

fn supported_version(program: Program<'_>) -> Option<SupportedErVersion> {
    let Program::Mapping(module) = program else {
        return None;
    };
    SupportedErVersion::detect(&module).ok()
}

fn install_tasks_once(version: SupportedErVersion) {
    INSTALL_TASKS.call_once(|| {
        let Ok(task_imp) = CSTaskImp::wait_for_instance(Duration::from_secs(30)) else {
            log_line("task scheduler was not available");
            return;
        };

        log_line(format_args!("game executable: {}", version.label()));
        if let Some(rvas) = version.native_rvas() {
            let _ = NATIVE_RVAS.set(rvas);
            // Installed always (each stub follows its flag), so the Hub's
            // No summoning prompts option can change at any time.
            let prompt_hooks = install_native_summon_prompt_hooks();
            NATIVE_SUMMON_PROMPT_HOOKS_READY.store(prompt_hooks, Ordering::Release);
            let do_summon_gate = prompt_hooks && install_native_do_summon_gate_hook();
            NATIVE_DO_SUMMON_GATE_READY.store(do_summon_gate, Ordering::Release);
            let cost_hooks = do_summon_gate && install_native_summon_cost_hooks();
            if do_summon_gate && !cost_hooks {
                log_line("native summon-cost hooks incomplete; additional Ashes may be summoned without their FP cost");
            }
            if !prompt_hooks || !do_summon_gate {
                log_line(format_args!(
                    "native summon-prompt hooks unavailable (prompt_hooks={prompt_hooks} do_summon_gate={do_summon_gate}); the replace prompt can still appear"
                ));
            }
            let exit_filter_installed = install_native_exit_filter();
            NATIVE_EXIT_FILTER_READY.store(exit_filter_installed, Ordering::Release);
            if !exit_filter_installed {
                log_line("native scoped-dismissal hook unavailable; selected dismissal will use the manual fade path");
            }

            let installed = install_native_clear_guard();
            NATIVE_CLEAR_GUARD_READY.store(installed, Ordering::Release);
            if !installed {
                log_line("native owner-group guard unavailable; pending switch forwarding disabled");
            }

            if hud_rows_active() {
                hud::install_native_hooks();
            }
        } else {
            log_line("native owner-group guard is only verified for WW 2.7.1.0 and 2.7.2.0; pending switch forwarding disabled");
        }

        let policy_task = task_imp.run_recurring(
            timed_spirit_ash_task as fn(&FD4TaskData),
            CSTaskGroupIndex::ChrIns_PostPhysics,
        );
        std::mem::forget(policy_task);

        // The inventory/UI can grey out Spirit Ashes before character physics
        // runs. Clear the vanilla gate at the game and menu steps as well as
        // before character updates; PostPhysics remains the fallback.
        for group in [
            CSTaskGroupIndex::GameMan,
            CSTaskGroupIndex::TaskLineIdx_InGame_InGameStep,
            CSTaskGroupIndex::TaskLineIdx_InGame_InGameMenuStep,
            CSTaskGroupIndex::ChrIns_PrePhysics_Begin,
        ] {
            let eligibility_task = task_imp.run_recurring(
                timed_allow_additional_ashes_task as fn(&FD4TaskData),
                group,
            );
            std::mem::forget(eligibility_task);
        }

        if matches!(
            version,
            SupportedErVersion::Ww2700
                | SupportedErVersion::Ww2710
                | SupportedErVersion::Ww2720
                | SupportedErVersion::Jp2721
        ) {
            let hud_task = task_imp.run_recurring(
                timed_hud_task as fn(&FD4TaskData),
                CSTaskGroupIndex::ScaleformStep,
            );
            std::mem::forget(hud_task);
            log_line("compact independent spirit ash HUD task registered at ScaleformStep (WW 2.7.0.0/2.7.1.0/2.7.2.0; 2.7.1.0 and 2.7.2.0 remain experimental)");
        } else {
            log_line("compact independent spirit ash HUD unavailable: FE layout is not enabled for this game version");
        }

        let settings = settings();
        log_line(format_args!(
            "initialised: build={}, native_exit_filter={}, native_clear_guard={}, native_prompt_hooks={}, native_do_summon_gate={}, max_active_ashes={} (0=unlimited), toggle_dismiss={}, patch_buddy_item_limits={}, suppress_spirit_ash_confirmation={}, suppress_additional_summon_prompt={}, clear_active_buddy_stone_link={}, temporary_pending_item_unlink={}, log_enabled={}",
            BUILD_VERSION,
            NATIVE_EXIT_FILTER_READY.load(Ordering::Acquire),
            NATIVE_CLEAR_GUARD_READY.load(Ordering::Acquire),
            NATIVE_SUMMON_PROMPT_HOOKS_READY.load(Ordering::Acquire),
            NATIVE_DO_SUMMON_GATE_READY.load(Ordering::Acquire),
            settings.max_active_ashes,
            settings.toggle_dismiss,
            settings.patch_buddy_item_limits,
            settings.suppress_spirit_ash_confirmation,
            settings.suppress_additional_summon_prompt,
            settings.clear_active_buddy_stone_link,
            settings.temporary_pending_item_unlink,
            settings.log_enabled
        ));
    });
}

fn install_native_clear_guard() -> bool {
    let program = Program::current();
    let Ok(call_site) = program
        .rva_to_va(native_rvas().clear_callsite)
        .map(|address| address as usize)
    else {
        log_line("native owner-group guard: call site RVA is outside the executable");
        return false;
    };
    let Ok(original) = program
        .rva_to_va(native_rvas().clear_function)
        .map(|address| address as usize)
    else {
        log_line("native owner-group guard: original function RVA is outside the executable");
        return false;
    };
    if call_site & 7 != 0 {
        log_line("native owner-group guard: call site is not 8-byte aligned");
        return false;
    }

    let expected = [0xe8, 0xeb, 0x2f, 0x00, 0x00, 0x41, 0x83, 0xbf];
    let actual = unsafe { std::slice::from_raw_parts(call_site as *const u8, expected.len()) };
    if actual != expected {
        log_line("native owner-group guard: call site signature mismatch; no code changed");
        return false;
    }
    let original_displacement = i32::from_le_bytes([actual[1], actual[2], actual[3], actual[4]]);
    let encoded_target = ((call_site + 5) as isize + original_displacement as isize) as usize;
    if encoded_target != original {
        log_line("native owner-group guard: call target does not match the verified function RVA");
        return false;
    }

    let Some(stub) = (unsafe { allocate_near(call_site) }) else {
        log_line("native owner-group guard: could not allocate a nearby call stub");
        return false;
    };
    let callback = guarded_clear_owner_groups as *const () as u64;
    let mut stub_code = [0u8; 12];
    stub_code[0..2].copy_from_slice(&[0x48, 0xb8]); // mov rax, callback
    stub_code[2..10].copy_from_slice(&callback.to_le_bytes());
    stub_code[10..12].copy_from_slice(&[0xff, 0xe0]); // jmp rax
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
        log_line("native owner-group guard: could not flush the call stub");
        return false;
    }

    let relative = (stub as isize).wrapping_sub((call_site + 5) as isize);
    let Ok(relative) = i32::try_from(relative) else {
        log_line("native owner-group guard: nearby stub is outside rel32 range");
        return false;
    };
    let mut patched_call = [0u8; 8];
    patched_call[0] = 0xe8;
    patched_call[1..5].copy_from_slice(&relative.to_le_bytes());
    patched_call[5..8].copy_from_slice(&expected[5..8]);

    let mut old_protection = PAGE_PROTECTION_FLAGS(0);
    if unsafe {
        VirtualProtect(
            call_site as *const c_void,
            patched_call.len(),
            PAGE_EXECUTE_READWRITE,
            &mut old_protection,
        )
    }
    .is_err()
    {
        log_line("native owner-group guard: could not make the call site writable");
        return false;
    }

    ORIGINAL_NATIVE_CLEAR_GROUPS.store(original as isize, Ordering::Release);
    NATIVE_CLEAR_GUARD_STUB.store(stub as isize, Ordering::Release);
    unsafe {
        (&*(call_site as *const AtomicU64))
            .store(u64::from_le_bytes(patched_call), Ordering::SeqCst);
    }
    let flushed = unsafe {
        FlushInstructionCache(
            GetCurrentProcess(),
            Some(call_site as *const c_void),
            patched_call.len(),
        )
        .is_ok()
    };
    let mut ignored_protection = PAGE_PROTECTION_FLAGS(0);
    let restored = unsafe {
        VirtualProtect(
            call_site as *const c_void,
            patched_call.len(),
            old_protection,
            &mut ignored_protection,
        )
        .is_ok()
    };
    if !restored {
        log_line("native owner-group guard: warning, executable page protection was not restored");
    }
    if !flushed {
        log_line(
            "native owner-group guard: call-site cache flush failed; switch forwarding remains disabled",
        );
        return false;
    }

    log_line(format_args!(
        "native owner-group guard installed: call_rva={:#x} original_rva={:#x}",
        native_rvas().clear_callsite,
        native_rvas().clear_function
    ));
    true
}

/// Detour the native owner-wide dismissal routine, but keep the original
/// implementation behind a trampoline. The callback marks every non-target
/// Ash group as already exiting for the duration of the native loop, so the
/// engine runs its normal exit effect only for the selected effect group.
fn install_native_exit_filter() -> bool {
    let program = Program::current();
    let Ok(function) = program
        .rva_to_va(native_rvas().exit_function)
        .map(|address| address as usize)
    else {
        log_line("native scoped-dismissal hook: function RVA is outside the executable");
        return false;
    };
    let Ok(filter_site) = program
        .rva_to_va(native_rvas().exit_group_filter)
        .map(|address| address as usize)
    else {
        log_line("native scoped-dismissal hook: group-filter RVA is outside the executable");
        return false;
    };
    if function & 7 != 0 {
        log_line("native scoped-dismissal hook: function entry is not 8-byte aligned");
        return false;
    }

    let actual_prologue = unsafe {
        std::slice::from_raw_parts(function as *const u8, NATIVE_EXIT_FUNCTION_PROLOGUE.len())
    };
    if actual_prologue != NATIVE_EXIT_FUNCTION_PROLOGUE {
        log_line(
            "native scoped-dismissal hook: function prologue signature mismatch; no code changed",
        );
        return false;
    }
    let actual_filter = unsafe {
        std::slice::from_raw_parts(
            filter_site as *const u8,
            NATIVE_EXIT_GROUP_FILTER_SIGNATURE.len(),
        )
    };
    if actual_filter != NATIVE_EXIT_GROUP_FILTER_SIGNATURE {
        log_line("native scoped-dismissal hook: group-filter signature mismatch; no code changed");
        return false;
    }

    let Some(page) = (unsafe { allocate_near(function) }) else {
        log_line("native scoped-dismissal hook: could not allocate a nearby trampoline page");
        return false;
    };
    let callback_stub = page.cast::<u8>();
    let trampoline = unsafe { callback_stub.add(0x40) };
    let callback = scoped_native_exit as *const () as u64;
    let mut callback_code = [0u8; 12];
    callback_code[0..2].copy_from_slice(&[0x48, 0xb8]); // mov rax, callback
    callback_code[2..10].copy_from_slice(&callback.to_le_bytes());
    callback_code[10..12].copy_from_slice(&[0xff, 0xe0]); // jmp rax
    unsafe {
        std::ptr::copy_nonoverlapping(callback_code.as_ptr(), callback_stub, callback_code.len());
    }

    let mut trampoline_code = [0u8; 13];
    trampoline_code[..NATIVE_EXIT_FUNCTION_PROLOGUE.len()]
        .copy_from_slice(&NATIVE_EXIT_FUNCTION_PROLOGUE);
    trampoline_code[8] = 0xe9;
    let return_relative = (function as isize + NATIVE_EXIT_FUNCTION_PROLOGUE.len() as isize)
        .wrapping_sub((trampoline as isize) + trampoline_code.len() as isize);
    let Ok(return_relative) = i32::try_from(return_relative) else {
        log_line("native scoped-dismissal hook: trampoline return is outside rel32 range");
        return false;
    };
    trampoline_code[9..13].copy_from_slice(&return_relative.to_le_bytes());
    unsafe {
        std::ptr::copy_nonoverlapping(trampoline_code.as_ptr(), trampoline, trampoline_code.len());
    }
    if unsafe {
        FlushInstructionCache(
            GetCurrentProcess(),
            Some(callback_stub.cast::<c_void>()),
            callback_code.len(),
        )
    }
    .is_err()
        || unsafe {
            FlushInstructionCache(
                GetCurrentProcess(),
                Some(trampoline.cast::<c_void>()),
                trampoline_code.len(),
            )
        }
        .is_err()
    {
        log_line("native scoped-dismissal hook: trampoline instruction-cache flush failed");
        return false;
    }

    let relative = (callback_stub as isize).wrapping_sub((function + 5) as isize);
    let Ok(relative) = i32::try_from(relative) else {
        log_line("native scoped-dismissal hook: callback stub is outside rel32 range");
        return false;
    };
    let mut patched_entry = [0x90u8; 8];
    patched_entry[0] = 0xe9; // jmp callback_stub
    patched_entry[1..5].copy_from_slice(&relative.to_le_bytes());

    let mut old_protection = PAGE_PROTECTION_FLAGS(0);
    if unsafe {
        VirtualProtect(
            function as *const c_void,
            patched_entry.len(),
            PAGE_EXECUTE_READWRITE,
            &mut old_protection,
        )
    }
    .is_err()
    {
        log_line("native scoped-dismissal hook: could not make the function entry writable");
        return false;
    }

    ORIGINAL_NATIVE_EXIT_ROUTINE.store(trampoline as isize, Ordering::Release);
    NATIVE_EXIT_FILTER_PAGE.store(page as isize, Ordering::Release);
    unsafe {
        (&*(function as *const AtomicU64))
            .store(u64::from_le_bytes(patched_entry), Ordering::SeqCst);
    }
    let flushed = unsafe {
        FlushInstructionCache(
            GetCurrentProcess(),
            Some(function as *const c_void),
            patched_entry.len(),
        )
        .is_ok()
    };
    let mut ignored_protection = PAGE_PROTECTION_FLAGS(0);
    let restored = unsafe {
        VirtualProtect(
            function as *const c_void,
            patched_entry.len(),
            old_protection,
            &mut ignored_protection,
        )
        .is_ok()
    };
    if !restored {
        log_line(
            "native scoped-dismissal hook: warning, function-page protection was not restored",
        );
    }
    if !flushed {
        log_line("native scoped-dismissal hook: function-entry cache flush failed after patch");
        return false;
    }

    log_line(format_args!(
        "native scoped-dismissal hook installed: function_rva={:#x} group_filter_rva={:#x}",
        native_rvas().exit_function,
        native_rvas().exit_group_filter
    ));
    true
}

unsafe fn allocate_near(call_site: usize) -> Option<*mut c_void> {
    // The original call uses rel32. Try free allocation-granularity slots on
    // both sides of the verified call site, staying well inside the 2 GiB range.
    for distance in (0x0800_0000usize..=0x7000_0000).step_by(0x0100_0000) {
        for candidate in [
            call_site.checked_add(distance),
            call_site.checked_sub(distance),
        ] {
            let Some(candidate) = candidate else {
                continue;
            };
            let hint = (candidate & !0xffff) as *const c_void;
            let page = unsafe {
                VirtualAlloc(
                    Some(hint),
                    0x1000,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_EXECUTE_READWRITE,
                )
            };
            if !page.is_null()
                && i32::try_from((page as isize).wrapping_sub((call_site + 5) as isize)).is_ok()
            {
                return Some(page);
            }
        }
    }
    None
}

unsafe extern "system" fn guarded_clear_owner_groups(
    manager_ptr: *mut SummonBuddyManager,
    owner_id: i32,
) {
    if manager_ptr.is_null() {
        return;
    }
    let manager = unsafe { &mut *manager_ptr };
    let request_effect = manager.request_summon_speffect_id;
    let pending_effect = PENDING_SWITCH_CLEAR_EFFECT.load(Ordering::Acquire) as i32;
    let requested_goods = manager.requested_summon_goods_id;
    let active_goods = manager.active_summon_goods_id;
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);

    let dismiss_effect = PENDING_ITEM_DISMISS_EFFECT.load(Ordering::Acquire) as i32;
    let dismiss_goods = PENDING_ITEM_DISMISS_GOODS.load(Ordering::Acquire) as i32;
    if NATIVE_CLEAR_GUARD_READY.load(Ordering::Acquire)
        && settings().enabled
        && dismiss_effect > 0
        && requested_goods == dismiss_goods
        && live_owner_has_effect(manager, owner_id, dismiss_effect)
    {
        let preserved_groups =
            clear_owner_group_exit_requests(manager, owner_id, Some(dismiss_effect));
        log_line(format_args!(
            "native owner-group clear bypassed: source=item_dismiss owner={} effect={} request_effect={} requested_goods={} active_effect={} active_goods={} preserved_other_groups={}",
            owner_id,
            dismiss_effect,
            request_effect,
            requested_goods,
            active_effect,
            active_goods,
            preserved_groups
        ));
        return;
    }

    if NATIVE_CLEAR_GUARD_READY.load(Ordering::Acquire)
        && settings().enabled
        && request_effect > 0
        && request_effect == pending_effect
        && requested_goods == PENDING_SWITCH_GOODS_ID.load(Ordering::Acquire) as i32
        && active_effect > 0
        && request_effect != active_effect
        && requested_goods > 0
        && active_goods > 0
    {
        let cap = settings().max_active_ashes;
        let existing = live_owner_ash_count(manager, owner_id, cap);
        if existing > 0 && (cap == 0 || existing < cap) {
            let preserved_groups = clear_owner_group_exit_requests(manager, owner_id, None);
            log_line(format_args!(
                "native owner-group clear bypassed: owner={} request_effect={} requested_goods={} active_goods={} existing_ashes={} cap={} preserved_groups={}",
                owner_id,
                request_effect,
                requested_goods,
                active_goods,
                existing,
                cap,
                preserved_groups
            ));
            return;
        }
    }

    let original = ORIGINAL_NATIVE_CLEAR_GROUPS.load(Ordering::Acquire);
    if original != 0 {
        let clear_groups: unsafe extern "system" fn(*mut SummonBuddyManager, i32) =
            unsafe { std::mem::transmute(original as usize) };
        unsafe { clear_groups(manager_ptr, owner_id) };
    }
}

/// The native owner-wide clear is reached after the engine may already have
/// marked every summon owned by this player for removal. When the clear is
/// bypassed, undo those marks immediately for all live groups except the Ash
/// the player explicitly dismissed. The regular task pass repeats this guard,
/// but running it here prevents unrelated groups from entering a frozen or
/// orphaned visual exit state during the same native update.
fn clear_owner_group_exit_requests(
    manager: &mut SummonBuddyManager,
    owner_id: i32,
    except_effect: Option<i32>,
) -> usize {
    let effect_by_buddy = manager
        .trigger_speffect_to_buddy_map
        .iter()
        .filter_map(|(effect, buddy)| (*effect > 0).then_some((*buddy, *effect)))
        .collect::<HashMap<_, _>>();
    let mut preserved_groups = 0usize;

    for entry in manager.groups.iter_mut() {
        if entry.first != owner_id {
            continue;
        }
        for group in entry.second.iter_mut() {
            if !chr_alive(unsafe { group.chr_ins.as_ref() })
                || effect_by_buddy
                    .get(&group.buddy_param_id)
                    .is_none_or(|effect| Some(*effect) == except_effect)
                || (!group.disappear_requested && group.disappear_delay_sec < 0.0)
            {
                continue;
            }

            group.disappear_requested = false;
            group.disappear_delay_sec = -1.0;
            preserved_groups += 1;
        }
    }

    preserved_groups
}

struct NativeExitGroupBackup {
    group: *mut SummonBuddyGroup,
    effect: Option<i32>,
    disappear_requested: bool,
    disappear_delay_sec: f32,
}

unsafe extern "system" fn scoped_native_exit(manager_ptr: *mut SummonBuddyManager) {
    if manager_ptr.is_null() {
        return;
    }
    let original = ORIGINAL_NATIVE_EXIT_ROUTINE.load(Ordering::Acquire);
    if original == 0 {
        return;
    }
    let call_number = NATIVE_EXIT_HOOK_CALLS.fetch_add(1, Ordering::AcqRel) + 1;
    let native_exit: unsafe extern "system" fn(*mut SummonBuddyManager) =
        unsafe { std::mem::transmute(original as usize) };

    if !NATIVE_EXIT_FILTER_READY.load(Ordering::Acquire) || !settings().enabled {
        unsafe { native_exit(manager_ptr) };
        return;
    }

    let manager = unsafe { &mut *manager_ptr };
    let buddy_to_trigger = buddy_trigger_map(manager);
    let living_effects = collect_all_live_effects(manager, &buddy_to_trigger);
    let pending_effect = PENDING_ITEM_DISMISS_EFFECT.load(Ordering::Acquire) as i32;
    let requested_goods = manager.requested_summon_goods_id;
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    let trace_context = pending_effect > 0 || manager.disappear_requested;
    let trace_key = if pending_effect > 0 {
        pending_effect as isize
    } else if manager.disappear_requested {
        isize::MIN + requested_goods.max(0) as isize
    } else {
        -1
    };
    if trace_context && NATIVE_EXIT_TRACE_LAST_KEY.swap(trace_key, Ordering::AcqRel) != trace_key {
        let mut live_effect_list = living_effects.iter().copied().collect::<Vec<_>>();
        live_effect_list.sort_unstable();
        log_line(format_args!(
            "native exit hook entered: call={} ready={} pending_effect={} request_effect={} requested_goods={} active_effect={} manager_disappear={} living_effects={:?}",
            call_number,
            NATIVE_EXIT_FILTER_READY.load(Ordering::Acquire),
            pending_effect,
            manager.request_summon_speffect_id,
            requested_goods,
            active_effect,
            manager.disappear_requested,
            live_effect_list
        ));
    } else if !trace_context {
        NATIVE_EXIT_TRACE_LAST_KEY.store(-1, Ordering::Release);
    }

    let mut target_effect =
        (pending_effect > 0 && living_effects.contains(&pending_effect)).then_some(pending_effect);
    if target_effect.is_none() && manager.disappear_requested {
        target_effect = buddy_to_trigger
            .values()
            .copied()
            .find(|effect| *effect == requested_goods && living_effects.contains(effect));
        if target_effect.is_none()
            && (requested_goods <= 0 || requested_goods == manager.active_summon_goods_id)
            && living_effects.contains(&active_effect)
        {
            target_effect = Some(active_effect);
        }
    }

    let Some(target_effect) = target_effect else {
        let pass_through_key = isize::MIN + pending_effect.max(0) as isize;
        if trace_context
            && NATIVE_EXIT_LAST_PASSTHROUGH_KEY.swap(pass_through_key, Ordering::AcqRel)
                != pass_through_key
        {
            log_line(format_args!(
                "native exit hook pass-through: call={} reason=no_matching_live_target pending_effect={} requested_goods={} active_effect={} manager_disappear={}",
                call_number,
                pending_effect,
                requested_goods,
                active_effect,
                manager.disappear_requested
            ));
        }
        unsafe { native_exit(manager_ptr) };
        return;
    };

    if pending_effect != target_effect {
        PENDING_ITEM_DISMISS_EFFECT.store(target_effect as isize, Ordering::Release);
        PENDING_ITEM_DISMISS_GOODS.store(
            if requested_goods > 0 {
                requested_goods as isize
            } else {
                manager.active_summon_goods_id as isize
            },
            Ordering::Release,
        );
        NATIVE_EXIT_TARGET_STARTED.store(-1, Ordering::Release);
    }

    let repeated_call =
        NATIVE_EXIT_TARGET_STARTED.load(Ordering::Acquire) == target_effect as isize;
    let previous_stone_entity = manager.active_summmon_buddy_stone_entity_id;
    let mut backups = Vec::new();
    let mut target_candidates = 0usize;
    let mut sibling_groups_skipped = 0usize;

    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            if !chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }
            let effect = buddy_to_trigger.get(&group.buddy_param_id).copied();
            if effect == Some(target_effect) {
                target_candidates += 1;
            } else {
                sibling_groups_skipped += 1;
            }
            backups.push(NativeExitGroupBackup {
                group: group as *mut SummonBuddyGroup,
                effect,
                disappear_requested: group.disappear_requested,
                disappear_delay_sec: group.disappear_delay_sec,
            });

            if effect == Some(target_effect) {
                if !repeated_call {
                    // The selected group must enter the native exit path even
                    // if a prior frame already marked it for generic removal.
                    group.disappear_requested = false;
                    group.disappear_delay_sec = -1.0;
                }
            } else {
                // Native 0x4b8160 skips a group whose +0x31 flag is already
                // set. Keeping this mask in place only during the call avoids
                // the owner-wide exit action on every other Ash.
                group.disappear_requested = true;
            }
        }
    }

    if target_candidates == 0 {
        if NATIVE_EXIT_LAST_PASSTHROUGH_KEY.swap(target_effect as isize, Ordering::AcqRel)
            != target_effect as isize
        {
            log_line(format_args!(
                "native exit hook pass-through: call={} reason=target_groups_not_alive_or_unmapped effect={} requested_goods={} living_effects={:?}",
                call_number, target_effect, requested_goods, living_effects
            ));
        }
        for backup in backups {
            let group = unsafe { &mut *backup.group };
            group.disappear_requested = backup.disappear_requested;
            group.disappear_delay_sec = backup.disappear_delay_sec;
        }
        unsafe { native_exit(manager_ptr) };
        return;
    }

    unsafe { native_exit(manager_ptr) };

    // The native function clears the manager's one active-stone link before
    // entering its owner loop. Restore it immediately; the dismissal request
    // belongs to one group and must not detach the surviving Ash's item state.
    let manager = unsafe { &mut *manager_ptr };
    manager.active_summmon_buddy_stone_entity_id = previous_stone_entity;

    let mut target_started = 0usize;
    for backup in backups {
        let group = unsafe { &mut *backup.group };
        if backup.effect == Some(target_effect) && !repeated_call {
            if group.disappear_requested
                && group.disappear_delay_sec.is_finite()
                && group.disappear_delay_sec >= 0.0
            {
                target_started += 1;
            } else {
                // If the native owner lookup did not include this group, undo
                // the temporary target reset instead of leaving it altered.
                group.disappear_requested = backup.disappear_requested;
                group.disappear_delay_sec = backup.disappear_delay_sec;
            }
        } else if backup.effect != Some(target_effect) {
            group.disappear_requested = backup.disappear_requested;
            group.disappear_delay_sec = backup.disappear_delay_sec;
        }
    }

    if target_started > 0 {
        NATIVE_EXIT_TARGET_STARTED.store(target_effect as isize, Ordering::Release);
    }
    if NATIVE_EXIT_LAST_SCOPED_LOG_EFFECT.swap(target_effect as isize, Ordering::AcqRel)
        != target_effect as isize
    {
        log_line(format_args!(
            "native dismissal scoped: effect={} requested_goods={} target_groups={} target_started={} sibling_groups_skipped={} repeated_call={}",
            target_effect,
            requested_goods,
            target_candidates,
            target_started,
            sibling_groups_skipped,
            repeated_call
        ));
    }
}

fn live_owner_ash_count(manager: &SummonBuddyManager, owner_id: i32, cap: usize) -> usize {
    let mut effects = [0i32; 256];
    let mut count = 0usize;
    for entry in manager.groups.iter() {
        if entry.first != owner_id {
            continue;
        }
        for group in entry.second.iter() {
            if !chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }
            let Some(effect) = summon_effect_for_buddy(manager, group.buddy_param_id) else {
                continue;
            };
            if effects[..count].contains(&effect) {
                continue;
            }
            if count == effects.len() {
                return count;
            }
            effects[count] = effect;
            count += 1;
            if cap > 0 && count >= cap {
                return count;
            }
        }
    }
    count
}

fn summon_effect_for_buddy(manager: &SummonBuddyManager, buddy_param_id: i32) -> Option<i32> {
    manager
        .trigger_speffect_to_buddy_map
        .iter()
        .find_map(|(effect, buddy)| (*effect > 0 && *buddy == buddy_param_id).then_some(*effect))
}

fn live_owner_has_effect(
    manager: &SummonBuddyManager,
    owner_id: i32,
    requested_effect: i32,
) -> bool {
    manager.groups.iter().any(|entry| {
        entry.first == owner_id
            && entry.second.iter().any(|group| {
                chr_alive(unsafe { group.chr_ins.as_ref() })
                    && summon_effect_for_buddy(manager, group.buddy_param_id)
                        == Some(requested_effect)
            })
    })
}

fn clear_pending_switch_markers(effect: i32) {
    if PENDING_SWITCH_CLEAR_EFFECT
        .compare_exchange(effect as isize, -1, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        PENDING_SWITCH_GOODS_ID.store(-1, Ordering::Release);
    }
}

fn synchronize_pending_switch_goods(
    manager: &mut SummonBuddyManager,
    active_effect: i32,
    living_effects: &HashSet<i32>,
    state: &mut RuntimeState,
) {
    let pending_effect = PENDING_SWITCH_CLEAR_EFFECT.load(Ordering::Acquire) as i32;
    let pending_goods = PENDING_SWITCH_GOODS_ID.load(Ordering::Acquire) as i32;
    if pending_effect <= 0
        || pending_goods <= 0
        || active_effect != pending_effect
        || !living_effects.contains(&pending_effect)
    {
        return;
    }
    if manager.active_summon_goods_id != pending_goods {
        let previous_goods = manager.active_summon_goods_id;
        manager.active_summon_goods_id = pending_goods;
        log_line(format_args!(
            "active summon goods synchronized: effect={} previous_goods={} active_goods={}",
            pending_effect, previous_goods, pending_goods
        ));
    }
    clear_pending_switch_markers(pending_effect);
    state.pending_switch_request_effect = None;
    state.pending_switch_request_goods = None;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemAction {
    Summon(i32),
    Dismiss(i32),
}

#[derive(Default)]
struct RuntimeState {
    initialized: bool,
    last_active_effect: i32,
    last_active_goods_id: i32,
    last_request_effect: i32,
    pending_requested_effect: Option<i32>,
    pending_switch_request_effect: Option<i32>,
    pending_switch_request_goods: Option<i32>,
    previous_live_effects: HashSet<i32>,
    dismissed_effects: HashSet<i32>,
    dismissal_fade_backups: HashMap<usize, DismissalFadeBackup>,
    dismissal_animation_ids: HashMap<usize, i32>,
    dismissal_visual_effects_applied: HashSet<usize>,
    native_exit_handoff: Option<NativeExitHandoff>,
    queued_native_dismissals: VecDeque<QueuedNativeDismissal>,
    last_item_cooldown_pacing_report: Option<Instant>,
    pending_stone_restore: Option<PendingStoneRestore>,
    summon_goods_by_effect: HashMap<i32, i32>,
    summon_dialog_originals: HashMap<i32, i32>,
    /// use_limit_summon_buddy values cleared by the item-limit patch, put back
    /// when NativeTweaks switches the feature off.
    item_limit_originals: HashMap<i32, u8>,
    summon_dialog_candidates_captured: bool,
    /// NativeTweaks: inputs of the last full pass over the dialog rows and its
    /// counts; the pass is skipped while they stay the same.
    dialog_rows_key: Option<u64>,
    dialog_rows_counts: (usize, usize, usize),
    last_summon_dialog_policy: String,
    next_report: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QueuedNativeDismissal {
    effect: i32,
    goods_id: i32,
    queued_at: Instant,
}

#[derive(Clone, Copy)]
struct DismissalFadeBackup {
    disabled_secondary_actions: bool,
    disabled_movement: bool,
    opacity_keyframes_multiplier: f32,
    opacity_keyframes_multiplier_previous: f32,
}

struct PendingStoneRestore {
    goods_id: i32,
    original_effect: i32,
    stone_entity_id: u32,
    started_at: Instant,
}

struct NativeExitHandoff {
    effect: i32,
    goods_id: i32,
    previous_active_effect_raw: u32,
    previous_active_goods_id: i32,
    previous_active_buddy_stone_entity_id: u32,
    started_at: Instant,
    animation_started: bool,
    animation_started_at: Option<Instant>,
}

fn pace_item_use_cooldown(
    manager: &mut SummonBuddyManager,
    state: &mut RuntimeState,
    source: &str,
) {
    let remaining = manager.item_use_cooldown_timer;
    if !remaining.is_finite() || remaining <= MIN_ITEM_ACTION_INTERVAL_SECONDS {
        return;
    }

    manager.item_use_cooldown_timer = MIN_ITEM_ACTION_INTERVAL_SECONDS;
    let now = Instant::now();
    if state
        .last_item_cooldown_pacing_report
        .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(500))
    {
        state.last_item_cooldown_pacing_report = Some(now);
        log_line(format_args!(
            "item cooldown paced: previous={:.2} minimum_interval={:.2} source={}",
            remaining, MIN_ITEM_ACTION_INTERVAL_SECONDS, source
        ));
    }
}

fn timed_spirit_ash_task(data: &FD4TaskData) {
    perf::measure(perf::POLICY, || spirit_ash_task(data));
}

fn timed_allow_additional_ashes_task(data: &FD4TaskData) {
    perf::measure(perf::ELIGIBILITY, || allow_additional_ashes_task(data));
}

fn timed_hud_task(data: &FD4TaskData) {
    perf::measure(perf::HUD, || hud::independent_ash_hud_task(data));
}

fn spirit_ash_task(_: &FD4TaskData) {
    if !settings().enabled {
        restore_native_rows_when_off();
        return;
    }

    let Ok(world) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    // The native binding models this field as a NonNull-owned pointer, but the
    // game can leave it null while the world is transitioning into a save.
    // Read its raw address before forming a Rust reference so that this early
    // transition cannot dereference a null manager and terminate the game.
    let Some(manager_ptr) = summon_buddy_manager_ptr(world) else {
        if !NULL_MANAGER_REPORTED.swap(true, Ordering::Relaxed) {
            log_line("waiting: summon buddy manager is not ready");
        }
        return;
    };

    let player_dead = world
        .main_player
        .as_ref()
        .is_none_or(|player| chr_dead(&player.chr_ins));
    let player_position = player_havok_position(world);
    let manager: &mut SummonBuddyManager = unsafe { &mut *manager_ptr };
    let buddy_to_trigger = buddy_trigger_map(manager);
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    let request_effect = manager.request_summon_speffect_id;
    let alive_flag_before_clear = manager.player_has_alive_summon;
    let disappear_before_clear = manager.disappear_requested;
    let live_effects = collect_live_effects(manager, &buddy_to_trigger);
    let all_live_effects = collect_all_live_effects(manager, &buddy_to_trigger);

    if !player_dead {
        clear_active_buddy_stone_link(manager, active_effect, &live_effects);
    }
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    // Remove the native summon-item restriction as soon as the manager and its
    // trigger map are available. Capture the original dialog IDs before changing
    // the item rows so the replacement prompt can be suppressed selectively.
    if !player_dead {
        clear_buddy_item_limits_once(
            manager,
            active_effect,
            manager.active_summon_goods_id,
            &mut state,
        );
    }
    extras::update_ash_cost(&state);
    update_summon_dialog_policy(manager, &all_live_effects, &mut state, "post_physics");
    extras::take_dismiss_all_request(manager, &buddy_to_trigger, &mut state);
    extras::update_buddy_stone_rules();
    extras::update_damage_by_count(manager, &buddy_to_trigger, &all_live_effects);
    if settings().follow_areas {
        update_area_follow(manager, &buddy_to_trigger, &state.dismissed_effects, player_position);
    }
    pace_item_use_cooldown(manager, &mut state, "spirit_ash_task_pre_action");
    synchronize_pending_switch_goods(manager, active_effect, &all_live_effects, &mut state);

    if !state.initialized {
        state.initialized = true;
        state.last_active_effect = active_effect;
        state.last_active_goods_id = manager.active_summon_goods_id;
        state.last_request_effect = request_effect;
        state.previous_live_effects = live_effects;
        state.next_report = Some(Instant::now() + Duration::from_secs(5));
        log_line(format_args!(
            "ready: active_effect={} active_ashes={}",
            active_effect,
            state.previous_live_effects.len()
        ));
        return;
    }

    if !player_dead {
        temporarily_unlink_for_pending_item(
            manager,
            active_effect,
            request_effect,
            &live_effects,
            &mut state,
        );
    }

    let request_changed = request_effect != state.last_request_effect;
    if request_changed {
        log_line(format_args!(
            "item request: effect={} goods={} manager_active={} active_goods={}",
            request_effect,
            manager.requested_summon_goods_id,
            active_effect,
            manager.active_summon_goods_id
        ));
        state.last_request_effect = request_effect;
        if request_effect > 0 {
            let living_effects = collect_all_live_effects(manager, &buddy_to_trigger);
            if living_effects.contains(&request_effect) {
                if !manager.disappear_requested {
                    log_line(format_args!(
                        "item dismissal deferred: source=post_physics effect={} reason=awaiting_confirmation",
                        request_effect
                    ));
                }
            } else {
                record_dismissal_target(
                    request_effect,
                    &living_effects,
                    &mut state.dismissed_effects,
                );
                sync_spawn_origin_for_request(
                    manager,
                    player_position,
                    request_effect,
                    manager.requested_summon_goods_id,
                    active_effect,
                    &buddy_to_trigger,
                    &collect_all_live_effects(manager, &buddy_to_trigger),
                    "post_physics",
                );
                arm_direct_summon_guard(
                    manager,
                    request_effect,
                    &collect_all_live_effects(manager, &buddy_to_trigger),
                    &mut state,
                    "post_physics",
                );
            }
        }
    }

    let live_before_native_dismissal = collect_all_live_effects(manager, &buddy_to_trigger);
    observe_confirmed_vanilla_dismissal(
        manager,
        &buddy_to_trigger,
        active_effect,
        request_effect,
        &live_before_native_dismissal,
        &mut state,
    );
    recover_scoped_native_handoff(manager, &buddy_to_trigger, &mut state);

    if !player_dead {
        let all_live_before_action = collect_all_live_effects(manager, &buddy_to_trigger);
        let requested_action = (request_changed && request_effect > 0)
            .then(|| {
                (!all_live_before_action.contains(&request_effect))
                    .then(|| {
                        classify_item_action(
                            request_effect,
                            &all_live_before_action,
                            settings().toggle_dismiss,
                        )
                    })
                    .flatten()
            })
            .flatten();
        let pending_effect_became_active = state
            .pending_requested_effect
            .is_some_and(|effect| all_live_before_action.contains(&effect));
        if pending_effect_became_active {
            state.pending_requested_effect = None;
        }
        let active_effect_fallback = (state.native_exit_handoff.is_none()
            && active_effect > 0
            && active_effect != state.last_active_effect
            && !all_live_before_action.contains(&active_effect)
            && !pending_effect_became_active)
            .then_some(ItemAction::Summon(active_effect));
        let mut action = requested_action.or(active_effect_fallback);
        let action_source = if requested_action.is_some() {
            "item_request"
        } else {
            "active_effect"
        };

        if let Some(ItemAction::Summon(effect)) = action {
            let cap = settings().max_active_ashes;
            if cap > 0
                && !all_live_before_action.contains(&effect)
                && all_live_before_action.len() >= cap
            {
                action = Some(ItemAction::Dismiss(effect));
                log_line(format_args!(
                    "cap reached: rejected trigger={} cap={} existing={}",
                    effect,
                    cap,
                    all_live_before_action.len()
                ));
            }
        }

        if let Some(action) = action {
            if let ItemAction::Dismiss(effect) = action {
                handle_live_item_dismissal(
                    manager,
                    &buddy_to_trigger,
                    effect,
                    &mut state,
                    "post_physics_action",
                );
            } else {
                apply_item_action(
                    manager,
                    &buddy_to_trigger,
                    action,
                    &mut state.dismissed_effects,
                );
            }
            if let ItemAction::Summon(effect) = action {
                state.pending_requested_effect = Some(effect);
            }
            // Keep the native manager flag only for an Ash the player
            // deliberately toggled off. A different Ash must not inherit the
            // game's one-slot replacement request.
            if !state.dismissed_effects.contains(&active_effect) {
                manager.disappear_requested = false;
            }
            let active_now = collect_live_effects(manager, &buddy_to_trigger);
            let living_now = collect_all_live_effects(manager, &buddy_to_trigger);
            log_line(format_args!(
                "item action: {:?} source={}; active_ashes={} living_ashes={} manager_active={}",
                action,
                action_source,
                active_now.len(),
                living_now.len(),
                active_effect
            ));
        }

        // The native summon update can mark existing groups for removal after
        // the item request has been handled. Re-apply the guard on each task
        // pass, while leaving an Ash explicitly selected for dismissal alone.
        protect_existing_groups(manager, &buddy_to_trigger, &state.dismissed_effects);
        apply_dismissal_visual_effect(manager, &buddy_to_trigger, &mut state);
        update_dismissal_fades(manager, &buddy_to_trigger, &mut state);
        pace_item_use_cooldown(manager, &mut state, "spirit_ash_task_post_action");

        // Vanilla derives this flag from its single active-summon slot. Clearing
        // it lets another Spirit Ash item reach the manager while older groups live.
        manager.player_has_alive_summon = false;
    }

    let all_live_groups = collect_all_live_effects(manager, &buddy_to_trigger);
    let completed_native_handoff = state
        .native_exit_handoff
        .as_ref()
        .filter(|handoff| !all_live_groups.contains(&handoff.effect))
        .map(|handoff| {
            (
                handoff.effect,
                handoff.goods_id,
                handoff.previous_active_effect_raw,
                handoff.previous_active_goods_id,
                handoff.previous_active_buddy_stone_entity_id,
                handoff.started_at.elapsed().as_millis(),
                handoff
                    .animation_started_at
                    .map(|started_at| started_at.elapsed().as_millis()),
            )
        });
    if let Some((
        effect,
        _,
        previous_active_effect_raw,
        previous_active_goods_id,
        previous_active_buddy_stone_entity_id,
        handoff_elapsed_ms,
        animation_elapsed_ms,
    )) = completed_native_handoff
    {
        restore_native_exit_active_slot(
            manager,
            &buddy_to_trigger,
            effect,
            previous_active_effect_raw,
            previous_active_goods_id,
            previous_active_buddy_stone_entity_id,
            "group_removed",
        );
        state.native_exit_handoff = None;
        log_line(format_args!(
            "native dismissal handoff completed: effect={} groups_remaining={} exit_hook_calls_during_handoff={} handoff_elapsed_ms={} animation_elapsed_ms={:?}",
            effect,
            all_live_groups.len(),
            NATIVE_EXIT_HOOK_CALLS
                .load(Ordering::Acquire)
                .saturating_sub(NATIVE_EXIT_HOOK_CALLS_AT_HANDOFF.load(Ordering::Acquire)),
            handoff_elapsed_ms,
            animation_elapsed_ms
        ));
    }
    state
        .dismissed_effects
        .retain(|effect| all_live_groups.contains(effect));
    if !player_dead {
        start_queued_native_dismissal(manager, &buddy_to_trigger, &mut state);
    }
    state.previous_live_effects = collect_live_effects(manager, &buddy_to_trigger);
    state.last_active_effect = state
        .native_exit_handoff
        .as_ref()
        .map(|handoff| i32::try_from(handoff.previous_active_effect_raw).unwrap_or(0))
        .unwrap_or_else(|| i32::try_from(manager.active_summon_speffect_id).unwrap_or(0));
    if state.native_exit_handoff.is_none()
        && request_effect <= 0
        && active_effect > 0
        && manager.active_summon_goods_id > 0
        && manager.requested_summon_goods_id == manager.active_summon_goods_id
    {
        state.last_active_goods_id = manager.active_summon_goods_id;
    }

    let now = Instant::now();
    if state.next_report.is_none_or(|at| now >= at) {
        let report_interval = if state.previous_live_effects.is_empty() {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(1)
        };
        state.next_report = Some(now + report_interval);
        log_line(format_args!(
            "state: alive_ashes={} active_effect={} active_effect_raw={} request_effect={} requested_goods={} active_goods={} active_buddy_stone={} player_dead={} alive_flag_before_clear={} disappear_before_clear={} manager_disappear_now={} item_cooldown={:.2} activation_range={} groups={}",
            state.previous_live_effects.len(),
            active_effect,
            manager.active_summon_speffect_id,
            request_effect,
            manager.requested_summon_goods_id,
            manager.active_summon_goods_id,
            manager.active_summmon_buddy_stone_entity_id,
            player_dead,
            alive_flag_before_clear,
            disappear_before_clear,
            manager.disappear_requested,
            manager.item_use_cooldown_timer,
            manager.is_within_activation_range,
            group_snapshot(manager, &buddy_to_trigger)
        ));
    }
}

/// Switched off from NativeTweaks: puts back the EquipParamGoods values the
/// feature changed, so vanilla summoning rules apply again. Switching it back
/// on patches them again on the next frame.
fn restore_native_rows_when_off() {
    reset_native_prompt_flags();
    extras::clear_dismiss_all_request();
    extras::restore_extra_ash_cost();
    extras::reset_damage_factor();
    if !BUDDY_ITEM_LIMITS_PATCHED.load(Ordering::Acquire) {
        return;
    }
    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let mut restored_limits = 0usize;
    for (goods_id, limit) in state.item_limit_originals.drain() {
        if let Some(row) = repository.get_mut::<EquipParamGoods>(goods_id as u32) {
            row.set_use_limit_summon_buddy(limit);
            restored_limits += 1;
        }
    }
    let mut restored_dialogs = 0usize;
    for (goods_id, dialog) in &state.summon_dialog_originals {
        if let Some(row) = repository.get_mut::<EquipParamGoods>(*goods_id as u32)
            && row.yes_no_dialog_message_id() != *dialog
        {
            row.set_yes_no_dialog_message_id(*dialog);
            restored_dialogs += 1;
        }
    }
    state.last_summon_dialog_policy.clear();
    state.dialog_rows_key = None;
    BUDDY_ITEM_LIMITS_PATCHED.store(false, Ordering::Release);
    log_line(format_args!(
        "switched off by NativeTweaks: restored use_limit_summon_buddy rows={restored_limits} summon prompt rows={restored_dialogs}"
    ));
}

fn clear_buddy_item_limits_once(
    manager: &SummonBuddyManager,
    active_effect: i32,
    active_goods: i32,
    state: &mut RuntimeState,
) {
    if !settings().patch_buddy_item_limits
        || BUDDY_ITEM_LIMITS_PATCHED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
    {
        return;
    }

    log_line(format_args!(
        "buddy item limits: patch_start before_first_summon active_effect={} active_goods={}",
        active_effect, active_goods
    ));

    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        BUDDY_ITEM_LIMITS_PATCHED.store(false, Ordering::Release);
        if !PARAM_REPOSITORY_WAIT_REPORTED.swap(true, Ordering::Relaxed) {
            log_line("buddy item limits: waiting for EquipParamGoods repository");
        }
        return;
    };

    let summon_effects: HashSet<i32> = manager
        .trigger_speffect_to_buddy_map
        .iter()
        .filter_map(|(trigger_effect, buddy_param_id)| {
            (*trigger_effect > 0 && *buddy_param_id > 0).then_some(*trigger_effect)
        })
        .collect();
    if summon_effects.is_empty() {
        BUDDY_ITEM_LIMITS_PATCHED.store(false, Ordering::Release);
        if !PARAM_TARGETS_WAIT_REPORTED.swap(true, Ordering::Relaxed) {
            log_line("buddy item limits: waiting for summon trigger effects");
        }
        return;
    }

    let mut scanned_rows = 0usize;
    let mut target_rows = 0usize;
    let mut patched_limit_rows = 0usize;
    let mut samples = Vec::new();
    for (goods_id, row) in repository.rows_mut::<EquipParamGoods>() {
        scanned_rows += 1;
        let Some(goods_id_i32) = i32::try_from(goods_id).ok() else {
            continue;
        };
        let previous_limit = row.use_limit_summon_buddy();
        let is_summon_goods = previous_limit != 0
            || summon_effects.contains(&goods_id_i32)
            || goods_id_i32 == active_goods
            || goods_id_i32 == manager.requested_summon_goods_id;
        if !is_summon_goods {
            continue;
        }
        target_rows += 1;
        state
            .summon_dialog_originals
            .entry(goods_id_i32)
            .or_insert_with(|| row.yes_no_dialog_message_id());
        if previous_limit != 0 {
            if samples.len() < 16 {
                samples.push(format!("{goods_id}:{previous_limit}"));
            }
            state
                .item_limit_originals
                .entry(goods_id_i32)
                .or_insert(previous_limit);
            row.set_use_limit_summon_buddy(0);
            patched_limit_rows += 1;
        }
    }

    if scanned_rows == 0 || target_rows == 0 {
        BUDDY_ITEM_LIMITS_PATCHED.store(false, Ordering::Release);
        if !PARAM_TARGETS_WAIT_REPORTED.swap(true, Ordering::Relaxed) {
            let mut effects = summon_effects.iter().copied().collect::<Vec<_>>();
            effects.sort_unstable();
            effects.truncate(12);
            log_line(format_args!(
                "buddy item limits: no matching EquipParamGoods rows; scanned={} trigger_effects={} sample_effects={:?} active_goods={}",
                scanned_rows,
                summon_effects.len(),
                effects,
                active_goods
            ));
        }
        return;
    }

    log_line(format_args!(
        "buddy item limits: scan_rows={} trigger_effects={} target_rows={} patched_limit_rows={} captured_dialogs={} samples={}",
        scanned_rows,
        summon_effects.len(),
        target_rows,
        patched_limit_rows,
        state.summon_dialog_originals.len(),
        if samples.is_empty() {
            "none".to_owned()
        } else {
            samples.join(",")
        }
    ));
    state.summon_dialog_candidates_captured = true;
}

fn update_summon_dialog_policy(
    manager: &SummonBuddyManager,
    live_effects: &HashSet<i32>,
    state: &mut RuntimeState,
    source: &str,
) {
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    let active_goods = manager.active_summon_goods_id;
    let requested_effect = manager.request_summon_speffect_id;
    let requested_goods = manager.requested_summon_goods_id;

    if active_effect > 0 && active_goods > 0 && live_effects.contains(&active_effect) {
        state
            .summon_goods_by_effect
            .insert(active_effect, active_goods);
    }
    if requested_effect > 0 && requested_goods > 0 && live_effects.contains(&requested_effect) {
        state
            .summon_goods_by_effect
            .insert(requested_effect, requested_goods);
    }
    state
        .summon_goods_by_effect
        .retain(|effect, _| live_effects.contains(effect));

    let mut active_goods_ids = state
        .summon_goods_by_effect
        .iter()
        .filter_map(|(effect, goods)| live_effects.contains(effect).then_some(*goods))
        .collect::<HashSet<_>>();
    if active_effect > 0 && active_goods > 0 && live_effects.contains(&active_effect) {
        active_goods_ids.insert(active_goods);
    }
    if requested_effect > 0 && requested_goods > 0 && live_effects.contains(&requested_effect) {
        active_goods_ids.insert(requested_goods);
    }

    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    if !state.summon_dialog_candidates_captured {
        let mut candidates = 0usize;
        for (goods_id, row) in repository.rows_mut::<EquipParamGoods>() {
            if row.use_limit_summon_buddy() == 0 {
                continue;
            }
            let Ok(goods_id) = i32::try_from(goods_id) else {
                continue;
            };
            state
                .summon_dialog_originals
                .entry(goods_id)
                .or_insert_with(|| row.yes_no_dialog_message_id());
            candidates += 1;
        }
        state.summon_dialog_candidates_captured = true;
        if candidates > 0 {
            log_line(format_args!(
                "summon prompt policy: candidate_rows_captured={candidates} source=use_limit_summon_buddy"
            ));
        }
    }

    let suppress_new_summon = settings().suppress_additional_summon_prompt;
    let suppress_all = settings().suppress_spirit_ash_confirmation;
    // The game first changes requested_summon_goods_id, then assigns
    // request_summon_speffect_id later in the item task. A different goods ID
    // that is not mapped to any living group identifies the pending summon.
    // Do not gate this on disappear_requested: vanilla raises that flag for
    // both replacement prompts and actual dismissals. The selected goods ID
    // and living-group mapping distinguish those paths.
    // Do not require the goods row to have been captured by the initial param
    // scan: some builds leave the selected row out of use_limit_summon_buddy,
    // while the game sources the prompt from the currently active Ash row.
    let requested_goods_is_living = active_goods_ids.contains(&requested_goods);
    let requested_goods_is_summon = state.summon_dialog_originals.contains_key(&requested_goods);
    let pending_new_summon_goods = (suppress_new_summon
        && !live_effects.is_empty()
        && requested_goods > 0
        && requested_goods != active_goods
        && !requested_goods_is_living)
        .then_some(requested_goods);
    let pending_new_summon = pending_new_summon_goods.is_some();
    if NATIVE_DO_SUMMON_GATE_READY.load(Ordering::Acquire) {
        update_native_prompt_flags(
            state,
            live_effects,
            active_goods,
            requested_goods,
            &active_goods_ids,
            suppress_new_summon,
            suppress_all,
            pending_new_summon_goods,
        );
    }
    let mut suppressed_rows = 0usize;
    let mut preserved_rows = 0usize;
    let mut restored_rows = 0usize;
    let mut additional_candidates = Vec::new();
    if active_effect > 0 && live_effects.contains(&active_effect) && active_goods > 0 {
        additional_candidates.push(active_goods);
    }
    if requested_effect > 0 && live_effects.contains(&requested_effect) && requested_goods > 0 {
        additional_candidates.push(requested_goods);
    }
    if let Some(goods_id) = pending_new_summon_goods {
        additional_candidates.push(goods_id);
    }
    additional_candidates.sort_unstable();
    additional_candidates.dedup();
    for goods_id in additional_candidates {
        if state.summon_dialog_originals.contains_key(&goods_id) || goods_id <= 0 {
            continue;
        }
        if let Some(row) = repository.get_mut::<EquipParamGoods>(goods_id as u32) {
            state
                .summon_dialog_originals
                .insert(goods_id, row.yes_no_dialog_message_id());
        }
    }

    // NativeTweaks: this ran over every Spirit Ash row (924 in 2.7.2.0) in each
    // of the 4-5 tasks per frame. With every prompt suppressed the result never
    // changes, so the pass only runs when what decides it changes.
    let rows_key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        suppress_all.hash(&mut hasher);
        suppress_new_summon.hash(&mut hasher);
        state.summon_dialog_originals.len().hash(&mut hasher);
        if !suppress_all {
            live_effects.is_empty().hash(&mut hasher);
            pending_new_summon.hash(&mut hasher);
            let mut living = active_goods_ids.iter().copied().collect::<Vec<_>>();
            living.sort_unstable();
            living.hash(&mut hasher);
        }
        hasher.finish()
    };
    let rows_unchanged = state.dialog_rows_key == Some(rows_key);
    if rows_unchanged {
        (suppressed_rows, preserved_rows, restored_rows) = state.dialog_rows_counts;
    }
    let candidate_rows = if rows_unchanged {
        Vec::new()
    } else {
        state
            .summon_dialog_originals
            .iter()
            .map(|(goods_id, original)| (*goods_id, *original))
            .collect::<Vec<_>>()
    };
    for (goods_id, original) in candidate_rows {
        let Some(row) = repository.get_mut::<EquipParamGoods>(goods_id as u32) else {
            continue;
        };
        // During a new-item request the vanilla game can source the
        // replacement dialog from the currently active Ash's goods row. Hide
        // all Ash confirmations only for that transition; when the selected
        // goods already belongs to a living group, retain its dismissal prompt.
        let keep_dismiss_confirmation = !pending_new_summon && active_goods_ids.contains(&goods_id);
        let suppress = suppress_all
            || (suppress_new_summon && !live_effects.is_empty() && !keep_dismiss_confirmation);
        let desired = if suppress { 0 } else { original };
        if suppress && original != 0 {
            suppressed_rows += 1;
        } else if keep_dismiss_confirmation && original != 0 {
            preserved_rows += 1;
        } else if !suppress && row.yes_no_dialog_message_id() != original && original != 0 {
            restored_rows += 1;
        }
        if row.yes_no_dialog_message_id() != desired {
            row.set_yes_no_dialog_message_id(desired);
        }
    }

    if !rows_unchanged {
        state.dialog_rows_key = Some(rows_key);
        state.dialog_rows_counts = (suppressed_rows, preserved_rows, restored_rows);
    }
    if !settings().log_enabled {
        return;
    }
    let mut living_goods = active_goods_ids.iter().copied().collect::<Vec<_>>();
    living_goods.sort_unstable();
    let status = format!(
        "live_effects={}:active_goods={active_goods}:requested_goods={requested_goods}:requested_effect={requested_effect}:manager_disappear_requested={}:requested_row_captured={requested_goods_is_summon}:requested_goods_is_living={requested_goods_is_living}:living_goods={living_goods:?}:pending_new_goods={}:protected_goods={}:suppressed_rows={suppressed_rows}:preserved_rows={preserved_rows}:restored_rows={restored_rows}:legacy_suppress_all={suppress_all}",
        live_effects.len(),
        manager.disappear_requested,
        pending_new_summon_goods.unwrap_or(0),
        active_goods_ids.len()
    );
    if state.last_summon_dialog_policy != status {
        log_line(format_args!(
            "summon prompt policy: source={source}:{status}"
        ));
        state.last_summon_dialog_policy = status;
    }
}

fn clear_active_buddy_stone_link(
    manager: &mut SummonBuddyManager,
    active_effect: i32,
    live_effects: &HashSet<i32>,
) {
    if !settings().clear_active_buddy_stone_link
        || active_effect <= 0
        || !live_effects.contains(&active_effect)
    {
        return;
    }

    let stone_entity_id = manager.active_summmon_buddy_stone_entity_id;
    if stone_entity_id == 0 {
        return;
    }

    // Preserve active_summon_speffect_id and active_summon_goods_id because
    // the HUD uses the active-summon state. Only detach the single-stone link.
    manager.active_summmon_buddy_stone_entity_id = 0;
    let previous_effect =
        LAST_CLEARED_BUDDY_STONE_EFFECT.swap(active_effect as isize, Ordering::Relaxed);
    if previous_effect != active_effect as isize {
        log_line(format_args!(
            "buddy stone link cleared: effect={} stone_entity={} living_ashes={}",
            active_effect,
            stone_entity_id,
            live_effects.len()
        ));
    }
}

fn allow_additional_ashes_task(data: &FD4TaskData) {
    if !settings().enabled {
        return;
    }

    let Ok(world) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(manager_ptr) = summon_buddy_manager_ptr(world) else {
        return;
    };

    let player_dead = world
        .main_player
        .as_ref()
        .is_none_or(|player| chr_dead(&player.chr_ins));
    if player_dead {
        return;
    }

    let manager = unsafe { &mut *manager_ptr };
    let buddy_to_trigger = buddy_trigger_map(manager);
    let all_live_effects = collect_all_live_effects(manager, &buddy_to_trigger);
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    let player_position = player_havok_position(world);
    clear_stale_pending_item_dismiss(manager, &all_live_effects);
    if let Ok(mut state) = STATE.lock() {
        clear_buddy_item_limits_once(
            manager,
            active_effect,
            manager.active_summon_goods_id,
            &mut state,
        );
        update_summon_dialog_policy(manager, &all_live_effects, &mut state, "early_item_task");
        pace_item_use_cooldown(manager, &mut state, "early_item_task");
        sync_spawn_origin_for_request(
            manager,
            player_position,
            manager.request_summon_speffect_id,
            manager.requested_summon_goods_id,
            active_effect,
            &buddy_to_trigger,
            &all_live_effects,
            "early_item_task",
        );
        synchronize_pending_switch_goods(manager, active_effect, &all_live_effects, &mut state);
        forward_pending_switch_request(
            manager,
            &buddy_to_trigger,
            &all_live_effects,
            &mut state,
            data.task_group_id,
        );

        let alive_flag = manager.player_has_alive_summon;
        let request_effect = manager.request_summon_speffect_id;
        let previous_request =
            LAST_ELIGIBILITY_REQUEST_EFFECT.swap(request_effect as isize, Ordering::Relaxed);
        if request_effect > 0 && previous_request != request_effect as isize {
            log_line(format_args!(
                "early item request: task_group={} effect={} goods={} active_effect={} active_goods={} active_buddy_stone={} disappear_requested={} alive_flag={} item_cooldown={:.2} activation_range={}",
                data.task_group_id,
                request_effect,
                manager.requested_summon_goods_id,
                manager.active_summon_speffect_id,
                manager.active_summon_goods_id,
                manager.active_summmon_buddy_stone_entity_id,
                manager.disappear_requested,
                alive_flag,
                manager.item_use_cooldown_timer,
                manager.is_within_activation_range
            ));
        }
        let request_changed = request_effect > 0 && previous_request != request_effect as isize;
        if request_changed {
            if all_live_effects.contains(&request_effect) {
                if settings().toggle_dismiss
                    && settings().suppress_spirit_ash_confirmation
                    && !manager.disappear_requested
                {
                    // NativeTweaks, No summoning prompts: the native Yes/No is
                    // hidden, so dismiss this Ash here, before the game turns
                    // the request into a summon that clears every group.
                    handle_live_item_dismissal(
                        manager,
                        &buddy_to_trigger,
                        request_effect,
                        &mut state,
                        "early_item_task_no_prompt",
                    );
                } else if !manager.disappear_requested {
                    log_line(format_args!(
                        "item dismissal deferred: source=early_item_task effect={} reason=awaiting_confirmation",
                        request_effect
                    ));
                }
            } else {
                record_dismissal_target(
                    request_effect,
                    &all_live_effects,
                    &mut state.dismissed_effects,
                );
                arm_direct_summon_guard(
                    manager,
                    request_effect,
                    &all_live_effects,
                    &mut state,
                    "early_item_task",
                );
            }
        }
        if !state.dismissed_effects.contains(&request_effect) {
            temporarily_unlink_for_pending_item(
                manager,
                active_effect,
                request_effect,
                &all_live_effects,
                &mut state,
            );
        }
        observe_confirmed_vanilla_dismissal(
            manager,
            &buddy_to_trigger,
            active_effect,
            request_effect,
            &all_live_effects,
            &mut state,
        );
        recover_scoped_native_handoff(manager, &buddy_to_trigger, &mut state);
        protect_existing_groups(manager, &buddy_to_trigger, &state.dismissed_effects);
    }
    let live_effects = collect_live_effects(manager, &buddy_to_trigger);
    clear_active_buddy_stone_link(manager, active_effect, &live_effects);
    manager.player_has_alive_summon = false;
}

fn forward_pending_switch_request(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    living_effects: &HashSet<i32>,
    state: &mut RuntimeState,
    task_group_id: u32,
) {
    if !NATIVE_CLEAR_GUARD_READY.load(Ordering::Acquire) {
        return;
    }

    let requested_goods = manager.requested_summon_goods_id;
    let active_goods = manager.active_summon_goods_id;
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);

    if let Some(pending_effect) = state.pending_switch_request_effect {
        let pending_goods = state.pending_switch_request_goods.unwrap_or(-1);
        let completed = active_effect == pending_effect
            && active_goods == pending_goods
            && living_effects.contains(&pending_effect);
        let cancelled_or_replaced = (requested_goods != pending_goods && requested_goods > 0)
            || (manager.request_summon_speffect_id > 0
                && manager.request_summon_speffect_id != pending_effect);
        if completed || cancelled_or_replaced {
            clear_pending_switch_markers(pending_effect);
            state.pending_switch_request_effect = None;
            state.pending_switch_request_goods = None;
        } else {
            return;
        }
    }

    if manager.request_summon_speffect_id > 0
        || !manager.disappear_requested
        || active_effect <= 0
        || requested_goods <= 0
        || active_goods <= 0
        || requested_goods == active_goods
        || requested_goods == active_effect
        || living_effects.contains(&requested_goods)
        || !living_effects.contains(&active_effect)
        || state.dismissed_effects.contains(&active_effect)
    {
        return;
    }

    let Some(target_effect) = buddy_to_trigger
        .values()
        .copied()
        .find(|effect| *effect == requested_goods)
    else {
        return;
    };

    // The game's confirmed switch path normally waits for the active group to
    // disappear before it writes the next effect request. Forward that request
    // now, while preserving the live group; the native manager still creates
    // the new Buddy group and updates its own active fields.
    PENDING_SWITCH_GOODS_ID.store(requested_goods as isize, Ordering::Release);
    PENDING_SWITCH_CLEAR_EFFECT.store(target_effect as isize, Ordering::Release);
    state.pending_switch_request_effect = Some(target_effect);
    state.pending_switch_request_goods = Some(requested_goods);
    manager.request_summon_speffect_id = target_effect;
    manager.disappear_requested = false;
    log_line(format_args!(
        "pending switch request forwarded: task_group={} goods={} effect={} active_effect={} active_goods={} groups_preserved={}",
        task_group_id,
        requested_goods,
        target_effect,
        active_effect,
        active_goods,
        living_effects.len()
    ));
}

fn arm_direct_summon_guard(
    manager: &SummonBuddyManager,
    target_effect: i32,
    living_effects: &HashSet<i32>,
    state: &mut RuntimeState,
    source: &str,
) {
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    let requested_goods = manager.requested_summon_goods_id;
    let cap = settings().max_active_ashes;
    if !NATIVE_CLEAR_GUARD_READY.load(Ordering::Acquire)
        || target_effect <= 0
        || living_effects.contains(&target_effect)
        || active_effect <= 0
        || !living_effects.contains(&active_effect)
        || state.dismissed_effects.contains(&active_effect)
        || requested_goods <= 0
        || (cap > 0 && living_effects.len() >= cap)
    {
        return;
    }

    if state.pending_switch_request_effect == Some(target_effect)
        && state.pending_switch_request_goods == Some(requested_goods)
    {
        return;
    }
    PENDING_SWITCH_GOODS_ID.store(requested_goods as isize, Ordering::Release);
    PENDING_SWITCH_CLEAR_EFFECT.store(target_effect as isize, Ordering::Release);
    state.pending_switch_request_effect = Some(target_effect);
    state.pending_switch_request_goods = Some(requested_goods);
    log_line(format_args!(
        "direct summon guard armed: source={} effect={} goods={} active_effect={} active_goods={} existing_ashes={} cap={}",
        source,
        target_effect,
        requested_goods,
        active_effect,
        manager.active_summon_goods_id,
        living_effects.len(),
        cap
    ));
}

fn player_havok_position(world: &WorldChrMan) -> Option<HavokPosition> {
    let player = world.main_player.as_ref()?;
    Some(player.chr_ins.modules.as_ref().physics.as_ref().position)
}

fn sync_spawn_origin_for_request(
    manager: &mut SummonBuddyManager,
    player_position: Option<HavokPosition>,
    request_effect: i32,
    requested_goods: i32,
    active_effect: i32,
    buddy_to_trigger: &HashMap<i32, i32>,
    living_effects: &HashSet<i32>,
    source: &str,
) {
    let requested_new_ash = (request_effect > 0 && !living_effects.contains(&request_effect))
        || (requested_goods > 0
            && requested_goods != manager.active_summon_goods_id
            && buddy_to_trigger
                .values()
                .any(|effect| *effect == requested_goods && !living_effects.contains(effect)));
    if !requested_new_ash {
        return;
    }
    let Some(position) = player_position else {
        return;
    };
    let previous = manager.spawn_origin;
    manager.spawn_origin = position;
    let target_effect = if request_effect > 0 {
        request_effect
    } else {
        buddy_to_trigger
            .values()
            .copied()
            .find(|effect| *effect == requested_goods)
            .unwrap_or(-1)
    };
    let previous_logged_effect =
        LAST_SPAWN_SYNC_EFFECT.swap(target_effect as isize, Ordering::Relaxed) as i32;
    if previous_logged_effect != target_effect
        || (previous.0 - position.0).abs() > 0.5
        || (previous.1 - position.1).abs() > 0.5
        || (previous.2 - position.2).abs() > 0.5
    {
        log_line(format_args!(
            "spawn origin synchronized: source={} target_effect={} active_effect={} request_effect={} requested_goods={} previous=({:.2},{:.2},{:.2}) player=({:.2},{:.2},{:.2})",
            source,
            target_effect,
            active_effect,
            request_effect,
            requested_goods,
            previous.0,
            previous.1,
            previous.2,
            position.0,
            position.1,
            position.2
        ));
    }
}

fn temporarily_unlink_for_pending_item(
    manager: &mut SummonBuddyManager,
    active_effect: i32,
    request_effect: i32,
    living_effects: &HashSet<i32>,
    state: &mut RuntimeState,
) {
    if !settings().temporary_pending_item_unlink {
        return;
    }

    if let Some(pending) = state.pending_stone_restore.take() {
        let request_accepted =
            request_effect > 0 && manager.requested_summon_goods_id == pending.goods_id;
        let item_became_active = manager.active_summon_goods_id == pending.goods_id;
        let item_cancelled = manager.requested_summon_goods_id == manager.active_summon_goods_id;
        let original_gone = !living_effects.contains(&pending.original_effect);
        let timed_out = pending.started_at.elapsed() >= Duration::from_secs(4);
        if request_accepted || item_became_active || item_cancelled || original_gone || timed_out {
            if !original_gone
                && manager.active_summmon_buddy_stone_entity_id == 0
                && (request_accepted || item_became_active || item_cancelled || timed_out)
            {
                manager.active_summmon_buddy_stone_entity_id = pending.stone_entity_id;
            }
            log_line(format_args!(
                "pending item stone link restored: goods={} original_effect={} request_effect={} active_goods={} original_alive={} timeout={} stone_entity={}",
                pending.goods_id,
                pending.original_effect,
                request_effect,
                manager.active_summon_goods_id,
                !original_gone,
                timed_out,
                manager.active_summmon_buddy_stone_entity_id
            ));
        } else {
            state.pending_stone_restore = Some(pending);
        }
        return;
    }

    let active_goods = manager.active_summon_goods_id;
    let requested_goods = manager.requested_summon_goods_id;
    let stone_entity_id = manager.active_summmon_buddy_stone_entity_id;
    if request_effect <= 0
        && active_effect > 0
        && living_effects.contains(&active_effect)
        && !manager.disappear_requested
        && active_goods > 0
        && requested_goods > 0
        && requested_goods != active_goods
        && stone_entity_id != 0
    {
        state.pending_stone_restore = Some(PendingStoneRestore {
            goods_id: requested_goods,
            original_effect: active_effect,
            stone_entity_id,
            started_at: Instant::now(),
        });
        manager.active_summmon_buddy_stone_entity_id = 0;
        log_line(format_args!(
            "pending item stone link temporarily cleared: goods={} active_effect={} active_goods={} stone_entity={}",
            requested_goods, active_effect, active_goods, stone_entity_id
        ));
    }
}

fn record_dismissal_target(
    requested_effect: i32,
    currently_living: &HashSet<i32>,
    dismissed_effects: &mut HashSet<i32>,
) {
    if requested_effect <= 0 {
        return;
    }

    if currently_living.contains(&requested_effect) {
        if dismissed_effects.insert(requested_effect) {
            log_line(format_args!(
                "dismiss target tracked: effect={} already_living=true",
                requested_effect
            ));
        }
    } else {
        dismissed_effects.remove(&requested_effect);
    }
}

fn handle_live_item_dismissal(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    target_effect: i32,
    state: &mut RuntimeState,
    source: &str,
) {
    if target_effect <= 0 {
        return;
    }

    if state
        .native_exit_handoff
        .as_ref()
        .is_some_and(|handoff| handoff.effect == target_effect)
    {
        log_line(format_args!(
            "item dismissal ignored: source={} effect={} reason=native_exit_handoff_pending",
            source, target_effect
        ));
        return;
    }

    let requested_goods = manager.requested_summon_goods_id;
    let request_was_pending = manager.request_summon_speffect_id == target_effect
        || PENDING_ITEM_DISMISS_EFFECT.load(Ordering::Acquire) as i32 != target_effect;
    state.dismissed_effects.insert(target_effect);
    PENDING_ITEM_DISMISS_EFFECT.store(target_effect as isize, Ordering::Release);
    PENDING_ITEM_DISMISS_GOODS.store(requested_goods as isize, Ordering::Release);

    let delay = DISMISS_FADE_SECONDS;
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            if !chr_alive(unsafe { group.chr_ins.as_ref() })
                || buddy_to_trigger.get(&group.buddy_param_id).copied() != Some(target_effect)
            {
                continue;
            }
            if group.disappear_requested
                && group.disappear_delay_sec.is_finite()
                && group.disappear_delay_sec >= 0.0
            {
                group.disappear_delay_sec = group.disappear_delay_sec.min(delay);
            } else {
                group.disappear_delay_sec = delay;
            }
            group.disappear_requested = true;
        }
    }

    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    manager.disappear_requested = false;
    if active_effect != target_effect
        && state.last_active_effect == active_effect
        && state.last_active_goods_id > 0
    {
        manager.active_summon_goods_id = state.last_active_goods_id;
    }
    manager.request_summon_speffect_id = -1;
    if request_was_pending {
        log_line(format_args!(
            "item dismissal redirected: source={} effect={} goods={} delay={:.2} active_effect={} active_goods={} groups={}",
            source,
            target_effect,
            requested_goods,
            delay,
            active_effect,
            manager.active_summon_goods_id,
            state.previous_live_effects.len()
        ));
    }
}

fn arm_native_exit_handoff(
    manager: &mut SummonBuddyManager,
    target_effect: i32,
    state: &mut RuntimeState,
    source: &str,
) -> bool {
    if target_effect <= 0 {
        return false;
    }

    if let Some(handoff) = state.native_exit_handoff.as_ref() {
        if handoff.effect == target_effect {
            return true;
        }
        log_line(format_args!(
            "native dismissal handoff busy: source={} effect={} pending_effect={} fallback=deferred",
            source, target_effect, handoff.effect
        ));
        return false;
    }

    let previous_active_effect_raw = manager.active_summon_speffect_id;
    let previous_active_effect = i32::try_from(previous_active_effect_raw).unwrap_or(0);
    let previous_active_goods_id = manager.active_summon_goods_id;
    let previous_active_buddy_stone_entity_id = manager.active_summmon_buddy_stone_entity_id;
    if !NATIVE_EXIT_FILTER_READY.load(Ordering::Acquire) {
        log_line(format_args!(
            "native dismissal handoff unavailable: source={} effect={} scoped_exit_ready=false fallback=fast_fade",
            source, target_effect
        ));
        return false;
    }
    let goods_id = if manager.requested_summon_goods_id > 0 {
        manager.requested_summon_goods_id
    } else {
        manager.active_summon_goods_id
    };
    let hook_calls_at_arm = NATIVE_EXIT_HOOK_CALLS.load(Ordering::Acquire);
    NATIVE_EXIT_HOOK_CALLS_AT_HANDOFF.store(hook_calls_at_arm, Ordering::Release);
    state.dismissed_effects.insert(target_effect);
    state.native_exit_handoff = Some(NativeExitHandoff {
        effect: target_effect,
        goods_id,
        previous_active_effect_raw,
        previous_active_goods_id,
        previous_active_buddy_stone_entity_id,
        started_at: Instant::now(),
        animation_started: false,
        animation_started_at: None,
    });
    PENDING_ITEM_DISMISS_EFFECT.store(target_effect as isize, Ordering::Release);
    PENDING_ITEM_DISMISS_GOODS.store(goods_id as isize, Ordering::Release);
    NATIVE_EXIT_TRACE_LAST_KEY.store(-1, Ordering::Release);
    NATIVE_EXIT_LAST_PASSTHROUGH_KEY.store(-1, Ordering::Release);
    NATIVE_EXIT_LAST_SCOPED_LOG_EFFECT.store(-1, Ordering::Release);
    if NATIVE_EXIT_TARGET_STARTED.load(Ordering::Acquire) != target_effect as isize {
        NATIVE_EXIT_TARGET_STARTED.store(-1, Ordering::Release);
    }
    // Vanilla only enters the native exit routine for the manager's active Ash.
    // Route that single slot through the selected group for the exit request;
    // the scoped native hook keeps every sibling group out of the removal.
    if previous_active_effect != target_effect {
        manager.active_summon_speffect_id = target_effect as u32;
        if goods_id > 0 {
            manager.active_summon_goods_id = goods_id;
        }
        log_line(format_args!(
            "native dismissal active slot redirected: previous_effect={} previous_goods={} target_effect={} target_goods={} source={} restore_after_target_removal=true",
            previous_active_effect, previous_active_goods_id, target_effect, goods_id, source
        ));
    }
    manager.disappear_requested = true;
    log_line(format_args!(
        "native dismissal handoff armed: source={} effect={} goods={} previous_active_effect={} previous_active_goods={} previous_active_buddy_stone={} exit_hook_calls_at_arm={} active_slot_redirected={} action=scoped_native_exit",
        source,
        target_effect,
        goods_id,
        previous_active_effect,
        previous_active_goods_id,
        previous_active_buddy_stone_entity_id,
        hook_calls_at_arm,
        previous_active_effect != target_effect
    ));
    true
}

fn restore_native_exit_active_slot(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    effect: i32,
    previous_active_effect_raw: u32,
    previous_active_goods_id: i32,
    previous_active_buddy_stone_entity_id: u32,
    reason: &str,
) {
    let current_active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    if current_active_effect != effect {
        log_line(format_args!(
            "native dismissal active slot restore skipped: effect={} current_effect={} reason={}",
            effect, current_active_effect, reason
        ));
        return;
    }

    let current_active_goods_id = manager.active_summon_goods_id;
    let remaining_effects = manager
        .groups
        .iter()
        .flat_map(|entry| entry.second.iter())
        .filter_map(|group| {
            if !chr_alive(unsafe { group.chr_ins.as_ref() }) || group.disappear_requested {
                return None;
            }
            buddy_to_trigger
                .get(&group.buddy_param_id)
                .copied()
                .filter(|remaining| *remaining != effect)
        })
        .collect::<HashSet<_>>();
    let previous_active_effect = i32::try_from(previous_active_effect_raw).unwrap_or(0);
    let (restored_effect, restored_goods, restored_stone, restore_source) =
        if previous_active_effect > 0
            && previous_active_effect != effect
            && remaining_effects.contains(&previous_active_effect)
        {
            (
                previous_active_effect_raw,
                previous_active_goods_id,
                previous_active_buddy_stone_entity_id,
                "previous_active_group",
            )
        } else if let Some(remaining_effect) = remaining_effects.iter().min().copied() {
            (
                remaining_effect as u32,
                remaining_effect,
                previous_active_buddy_stone_entity_id,
                "remaining_group",
            )
        } else {
            (
                previous_active_effect_raw,
                previous_active_goods_id,
                0,
                "vanilla_empty_slot",
            )
        };
    manager.active_summon_speffect_id = restored_effect;
    manager.active_summon_goods_id = restored_goods;
    manager.active_summmon_buddy_stone_entity_id = restored_stone;
    manager.disappear_requested = false;
    // NativeTweaks: the game leaves the dismissed Ash's goods as the requested
    // goods, so the next use of that item reads as "the active Ash" and is
    // spent on a dismissal of nothing (the player's first re-summon showed no
    // Ash). Point it at the Ash now in the active slot, as the queued path does.
    let stale_requested_goods = manager.requested_summon_goods_id;
    let requested_goods_reset = restore_source != "vanilla_empty_slot"
        && restored_goods > 0
        && (stale_requested_goods == current_active_goods_id || stale_requested_goods == effect);
    if requested_goods_reset {
        manager.requested_summon_goods_id = restored_goods;
    }
    log_line(format_args!(
        "native dismissal active slot restored: removed_effect={} current_goods={} active_effect={} active_goods={} active_buddy_stone={} source={} reason={} requested_goods={}{}",
        effect,
        current_active_goods_id,
        i32::try_from(restored_effect).unwrap_or(0),
        restored_goods,
        restored_stone,
        restore_source,
        reason,
        manager.requested_summon_goods_id,
        if requested_goods_reset {
            format!(" (was {stale_requested_goods})")
        } else {
            String::new()
        }
    ));
}

fn clear_stale_pending_item_dismiss(
    manager: &mut SummonBuddyManager,
    living_effects: &HashSet<i32>,
) {
    let pending_effect = PENDING_ITEM_DISMISS_EFFECT.load(Ordering::Acquire) as i32;
    if pending_effect <= 0
        || manager.request_summon_speffect_id > 0
        || living_effects.contains(&pending_effect)
    {
        return;
    }
    let pending_goods = PENDING_ITEM_DISMISS_GOODS.load(Ordering::Acquire) as i32;
    if manager.requested_summon_goods_id == pending_goods && manager.active_summon_goods_id > 0 {
        manager.requested_summon_goods_id = manager.active_summon_goods_id;
    }
    if PENDING_ITEM_DISMISS_EFFECT
        .compare_exchange(
            pending_effect as isize,
            -1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        )
        .is_ok()
    {
        PENDING_ITEM_DISMISS_GOODS.store(-1, Ordering::Release);
        NATIVE_EXIT_TARGET_STARTED.store(-1, Ordering::Release);
    }
}

fn start_queued_native_dismissal(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    state: &mut RuntimeState,
) {
    if state.native_exit_handoff.is_some() {
        return;
    }

    loop {
        let Some(queued) = state.queued_native_dismissals.pop_front() else {
            return;
        };
        let living_effects = collect_all_live_effects(manager, buddy_to_trigger);
        if !living_effects.contains(&queued.effect) {
            log_line(format_args!(
                "native dismissal queue entry discarded: effect={} goods={} reason=group_no_longer_live queue_wait_ms={} queue_len={}",
                queued.effect,
                queued.goods_id,
                queued.queued_at.elapsed().as_millis(),
                state.queued_native_dismissals.len()
            ));
            continue;
        }

        let previous_requested_goods = manager.requested_summon_goods_id;
        manager.disappear_requested = false;
        manager.requested_summon_goods_id = queued.goods_id;
        if arm_native_exit_handoff(manager, queued.effect, state, "queued_after_previous_exit") {
            log_line(format_args!(
                "native dismissal queue started: effect={} goods={} queue_wait_ms={} queue_remaining={}",
                queued.effect,
                queued.goods_id,
                queued.queued_at.elapsed().as_millis(),
                state.queued_native_dismissals.len()
            ));
            return;
        }

        if state.native_exit_handoff.is_some() {
            manager.requested_summon_goods_id = previous_requested_goods;
            state.queued_native_dismissals.push_front(queued);
            return;
        }

        log_line(format_args!(
            "native dismissal queue fallback: effect={} goods={} path=fast_fade",
            queued.effect, queued.goods_id
        ));
        handle_live_item_dismissal(
            manager,
            buddy_to_trigger,
            queued.effect,
            state,
            "queued_native_exit_unavailable",
        );
        if manager.active_summon_goods_id > 0 {
            manager.requested_summon_goods_id = manager.active_summon_goods_id;
        }
        return;
    }
}

fn observe_confirmed_vanilla_dismissal(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    active_effect: i32,
    request_effect: i32,
    currently_living: &HashSet<i32>,
    state: &mut RuntimeState,
) {
    // The native manager has one active-Ash slot, but the game keeps the
    // currently selected goods ID while the player confirms a dismissal.
    // When that goods ID belongs to a living Ash, use it as the target instead
    // of always dismissing the manager's last active Ash. This lets a player
    // remove a non-active Ash without allowing the native owner-wide clear.
    if manager.disappear_requested {
        let requested_goods = manager.requested_summon_goods_id;
        let selected_living_effect = buddy_to_trigger
            .values()
            .copied()
            .find(|effect| *effect == requested_goods && currently_living.contains(effect));
        if let Some(target_effect) = selected_living_effect {
            if !state.dismissed_effects.contains(&target_effect) {
                log_line(format_args!(
                    "native dismissal target observed: source=selected_goods effect={} manager_effect={} pending_goods={} active_goods={}",
                    target_effect, active_effect, requested_goods, manager.active_summon_goods_id
                ));
            }
            let armed =
                arm_native_exit_handoff(manager, target_effect, state, "manager_selected_goods");
            if !armed {
                if let Some(active_handoff) = state.native_exit_handoff.as_ref() {
                    if active_handoff.effect != target_effect {
                        let queued = QueuedNativeDismissal {
                            effect: target_effect,
                            goods_id: if requested_goods > 0 {
                                requested_goods
                            } else {
                                target_effect
                            },
                            queued_at: Instant::now(),
                        };
                        let already_queued = state
                            .queued_native_dismissals
                            .iter()
                            .any(|pending| pending.effect == queued.effect);
                        if !already_queued {
                            state.queued_native_dismissals.push_back(queued);
                            log_line(format_args!(
                                "native dismissal queued: effect={} goods={} after_effect={} queue_len={}",
                                queued.effect,
                                queued.goods_id,
                                active_handoff.effect,
                                state.queued_native_dismissals.len()
                            ));
                        }
                        manager.disappear_requested = false;
                        if manager.requested_summon_goods_id == requested_goods
                            && manager.active_summon_goods_id > 0
                        {
                            manager.requested_summon_goods_id = manager.active_summon_goods_id;
                        }
                        return;
                    }
                } else {
                    handle_live_item_dismissal(
                        manager,
                        buddy_to_trigger,
                        target_effect,
                        state,
                        "manager_selected_goods",
                    );
                }
            }
            return;
        }
    }

    // The game also sets the manager's disappear flag while switching from one
    // Spirit Ash item to another. That is a replacement path, not the player's
    // request to dismiss the currently active Ash. Goods IDs remain different
    // during this transition even when request_summon_speffect_id is still -1.
    let switching_to_another_ash = request_effect <= 0
        && manager.requested_summon_goods_id > 0
        && manager.active_summon_goods_id > 0
        && manager.requested_summon_goods_id != manager.active_summon_goods_id;
    if switching_to_another_ash {
        state.dismissed_effects.remove(&active_effect);
        report_preserved_effect(active_effect, "pending_ash_switch");
        return;
    }

    // The game does not always write request_summon_speffect_id when the
    // player confirms dismissal of the active Ash. A positive item request
    // is handled by the item path; this branch converts the manager flag into
    // a per-effect dismissal request.
    if request_effect > 0
        || !manager.disappear_requested
        || active_effect <= 0
        || !currently_living.contains(&active_effect)
    {
        return;
    }

    if state.dismissed_effects.insert(active_effect) {
        log_line(format_args!(
            "native dismissal target observed: source=manager effect={} pending_effect={} pending_goods={} active_goods={}",
            active_effect,
            request_effect,
            manager.requested_summon_goods_id,
            manager.active_summon_goods_id
        ));
    }

    // Leave the confirmed active-summon request for one native manager update.
    // The owner-group guard prevents that update from removing every group;
    // update_dismissal_fades will keep the engine's exit animation if it starts
    // and fall back to the short manual fade if it does not.
    arm_native_exit_handoff(manager, active_effect, state, "manager_confirm");
}

fn protect_existing_groups(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    dismissed_effects: &HashSet<i32>,
) {
    let active_effect = i32::try_from(manager.active_summon_speffect_id).unwrap_or(0);
    if manager.disappear_requested && !dismissed_effects.contains(&active_effect) {
        manager.disappear_requested = false;
        report_preserved_effect(active_effect, "manager");
    }

    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                continue;
            };
            if !chr_alive(unsafe { group.chr_ins.as_ref() })
                || dismissed_effects.contains(&effect)
                || PENDING_ITEM_DISMISS_EFFECT.load(Ordering::Acquire) as i32 == effect
                || (!group.disappear_requested && group.disappear_delay_sec < 0.0)
            {
                continue;
            }

            group.disappear_requested = false;
            group.disappear_delay_sec = -1.0;
            report_preserved_effect(effect, "group");
        }
    }
}

fn recover_scoped_native_handoff(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    state: &mut RuntimeState,
) {
    if state.native_exit_handoff.is_some() {
        return;
    }
    let effect = PENDING_ITEM_DISMISS_EFFECT.load(Ordering::Acquire) as i32;
    if effect <= 0 || NATIVE_EXIT_TARGET_STARTED.load(Ordering::Acquire) != effect as isize {
        return;
    }
    let target_is_exiting = manager.groups.iter().any(|entry| {
        entry.second.iter().any(|group| {
            buddy_to_trigger.get(&group.buddy_param_id).copied() == Some(effect)
                && group.disappear_requested
                && group.disappear_delay_sec.is_finite()
                && group.disappear_delay_sec >= 0.0
                && chr_alive(unsafe { group.chr_ins.as_ref() })
        })
    });
    if !target_is_exiting {
        return;
    }

    let goods_id = PENDING_ITEM_DISMISS_GOODS.load(Ordering::Acquire) as i32;
    let previous_active_effect_raw = manager.active_summon_speffect_id;
    let previous_active_effect = i32::try_from(previous_active_effect_raw).unwrap_or(0);
    let previous_active_goods_id = manager.active_summon_goods_id;
    let previous_active_buddy_stone_entity_id = manager.active_summmon_buddy_stone_entity_id;
    NATIVE_EXIT_HOOK_CALLS_AT_HANDOFF.store(
        NATIVE_EXIT_HOOK_CALLS.load(Ordering::Acquire),
        Ordering::Release,
    );
    state.dismissed_effects.insert(effect);
    state.native_exit_handoff = Some(NativeExitHandoff {
        effect,
        goods_id,
        previous_active_effect_raw,
        previous_active_goods_id,
        previous_active_buddy_stone_entity_id,
        started_at: Instant::now(),
        animation_started: false,
        animation_started_at: None,
    });
    log_line(format_args!(
        "native dismissal handoff recovered: effect={} goods={} active_effect={} active_goods={} source=scoped_native_exit",
        effect, goods_id, previous_active_effect, previous_active_goods_id
    ));
}

fn update_dismissal_fades(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    state: &mut RuntimeState,
) {
    let pending_handoff = state.native_exit_handoff.as_ref().map(|handoff| {
        (
            handoff.effect,
            handoff.goods_id,
            handoff.previous_active_effect_raw,
            handoff.previous_active_goods_id,
            handoff.previous_active_buddy_stone_entity_id,
            handoff.started_at,
            handoff.animation_started,
        )
    });

    if let Some((
        effect,
        goods_id,
        previous_active_effect_raw,
        previous_active_goods_id,
        previous_active_buddy_stone_entity_id,
        started_at,
        false,
    )) = pending_handoff
    {
        let native_exit = manager.groups.iter().find_map(|entry| {
            entry.second.iter().find_map(|group| {
                if buddy_to_trigger.get(&group.buddy_param_id).copied() != Some(effect)
                    || !group.disappear_requested
                    || !chr_alive(unsafe { group.chr_ins.as_ref() })
                {
                    return None;
                }
                let (animation_id, animation_time, animation_length) =
                    current_animation(unsafe { group.chr_ins.as_ref() });
                (animation_id.rem_euclid(1_000_000) == NATIVE_EXIT_ANIMATION_ID
                    && animation_length.is_finite()
                    && animation_length > 0.0)
                    .then_some((
                        group.buddy_param_id,
                        animation_id,
                        animation_time,
                        animation_length,
                    ))
            })
        });

        if let Some((buddy, animation_id, animation_time, animation_length)) = native_exit {
            if let Some(handoff) = state.native_exit_handoff.as_mut() {
                handoff.animation_started = true;
                if handoff.animation_started_at.is_none() {
                    handoff.animation_started_at = Some(Instant::now());
                }
            }
            log_line(format_args!(
                "native dismissal active slot restoration deferred: effect={} active_effect={} reason=waiting_for_group_removal",
                effect,
                i32::try_from(manager.active_summon_speffect_id).unwrap_or(0)
            ));

            let mut shortened_groups = 0usize;
            for entry in manager.groups.iter_mut() {
                for group in entry.second.iter_mut() {
                    if buddy_to_trigger.get(&group.buddy_param_id).copied() != Some(effect)
                        || !group.disappear_requested
                        || !chr_alive(unsafe { group.chr_ins.as_ref() })
                    {
                        continue;
                    }
                    let (group_animation, group_time, group_length) =
                        current_animation(unsafe { group.chr_ins.as_ref() });
                    if group_animation.rem_euclid(1_000_000) != NATIVE_EXIT_ANIMATION_ID
                        || !group_length.is_finite()
                        || group_length <= 0.0
                    {
                        continue;
                    }

                    // NativeTweaks: the native exit animation (1840) runs ~6.7 s;
                    // with fast dismissal remove the Ash after the same 1.5 s
                    // fade as the direct dismissal (update_dismissal_fades).
                    let animation_remaining = (group_length - group_time).max(0.0)
                        + NATIVE_EXIT_REMOVE_MARGIN_SECONDS;
                    let native_remaining = if settings().fast_dismissal {
                        animation_remaining.min(DISMISS_FADE_SECONDS)
                    } else {
                        animation_remaining
                    };
                    if group.disappear_delay_sec.is_finite() && group.disappear_delay_sec >= 0.0 {
                        group.disappear_delay_sec = group.disappear_delay_sec.min(native_remaining);
                    } else {
                        group.disappear_delay_sec = native_remaining;
                    }
                    shortened_groups += 1;
                }
            }
            log_line(format_args!(
                "native dismissal animation started: effect={} goods={} buddy={} anim={} time={:.2}/{:.2} groups_timed_to_animation_end={}",
                effect,
                goods_id,
                buddy,
                animation_id,
                animation_time,
                animation_length,
                shortened_groups
            ));
        } else if started_at.elapsed() >= NATIVE_EXIT_HANDOFF_TIMEOUT {
            log_line(format_args!(
                "native dismissal animation not observed: effect={} goods={} timeout_ms={} fallback=fast_fade",
                effect,
                goods_id,
                NATIVE_EXIT_HANDOFF_TIMEOUT.as_millis()
            ));
            restore_native_exit_active_slot(
                manager,
                buddy_to_trigger,
                effect,
                previous_active_effect_raw,
                previous_active_goods_id,
                previous_active_buddy_stone_entity_id,
                "animation_timeout",
            );
            state.native_exit_handoff = None;
            handle_live_item_dismissal(
                manager,
                buddy_to_trigger,
                effect,
                state,
                "native_exit_timeout_fallback",
            );
        }
    }

    let expired_handoff = state
        .native_exit_handoff
        .as_ref()
        .filter(|handoff| {
            handoff.animation_started && handoff.started_at.elapsed() >= NATIVE_EXIT_CLEANUP_TIMEOUT
        })
        .map(|handoff| {
            (
                handoff.effect,
                handoff.previous_active_effect_raw,
                handoff.previous_active_goods_id,
                handoff.previous_active_buddy_stone_entity_id,
            )
        });
    if let Some((
        effect,
        previous_active_effect_raw,
        previous_active_goods_id,
        previous_active_buddy_stone_entity_id,
    )) = expired_handoff
    {
        restore_native_exit_active_slot(
            manager,
            buddy_to_trigger,
            effect,
            previous_active_effect_raw,
            previous_active_goods_id,
            previous_active_buddy_stone_entity_id,
            "group_cleanup_timeout",
        );
        state.native_exit_handoff = None;
        log_line(format_args!(
            "native dismissal group cleanup timed out: effect={} timeout_ms={} action=restore_active_slot",
            effect,
            NATIVE_EXIT_CLEANUP_TIMEOUT.as_millis()
        ));
    }
    let native_exit_effect = state
        .native_exit_handoff
        .as_ref()
        .filter(|handoff| handoff.animation_started)
        .map(|handoff| handoff.effect);
    let RuntimeState {
        dismissed_effects,
        dismissal_fade_backups: fade_backups,
        dismissal_animation_ids: animation_ids,
        ..
    } = state;
    let mut live_characters = HashSet::new();
    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let character_key = group.chr_ins.as_ptr() as usize;
            live_characters.insert(character_key);
            let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                if let Some(backup) = fade_backups.remove(&character_key) {
                    restore_dismissal_fade(group, backup);
                }
                animation_ids.remove(&character_key);
                continue;
            };
            if !dismissed_effects.contains(&effect)
                || !group.disappear_requested
                || !chr_alive(unsafe { group.chr_ins.as_ref() })
            {
                if let Some(backup) = fade_backups.remove(&character_key) {
                    restore_dismissal_fade(group, backup);
                }
                animation_ids.remove(&character_key);
                continue;
            }

            let remaining = group.disappear_delay_sec.max(0.0);
            let alpha = (remaining / DISMISS_FADE_SECONDS).clamp(0.0, 1.0);
            let chr = unsafe { group.chr_ins.as_mut() };
            let (animation_id, animation_time, animation_length) = current_animation(chr);
            if native_exit_effect == Some(effect) {
                if animation_ids.insert(character_key, animation_id) != Some(animation_id) {
                    log_line(format_args!(
                        "dismissal animation trace: effect={} buddy={} anim={} time={:.2}/{:.2} delay={:.2} native_exit=true",
                        effect,
                        group.buddy_param_id,
                        animation_id,
                        animation_time,
                        animation_length,
                        remaining
                    ));
                }
                // NativeTweaks: with fast dismissal fall through to the opacity
                // fade below, which covers the shortened native exit as well.
                if !settings().fast_dismissal {
                    if let Some(backup) = fade_backups.remove(&character_key) {
                        restore_dismissal_fade(group, backup);
                    }
                    continue;
                }
            }
            if animation_ids.insert(character_key, animation_id) != Some(animation_id) {
                log_line(format_args!(
                    "dismissal animation trace: effect={} buddy={} anim={} time={:.2}/{:.2} delay={:.2} fade_alpha={:.2}",
                    effect,
                    group.buddy_param_id,
                    animation_id,
                    animation_time,
                    animation_length,
                    remaining,
                    alpha
                ));
            }
            fade_backups
                .entry(character_key)
                .or_insert(DismissalFadeBackup {
                    disabled_secondary_actions: chr.debug_flags.disabled_secondary_actions(),
                    disabled_movement: chr.debug_flags.disabled_movement(),
                    opacity_keyframes_multiplier: chr.opacity_keyframes_multiplier,
                    opacity_keyframes_multiplier_previous: chr
                        .opacity_keyframes_multiplier_previous,
                });
            // Keep character, animation, and render updates running so the opacity
            // multiplier reaches the renderer. Block new AI actions and movement
            // inputs without freezing the character's update pipeline.
            chr.debug_flags.set_disabled_secondary_actions(true);
            chr.debug_flags.set_disabled_movement(true);
            if remaining <= DISMISS_VFX_RELEASE_REMAINING_SECONDS {
                let effect_is_active = chr
                    .special_effect
                    .as_ref()
                    .entries()
                    .any(|entry| entry.param_id == DISMISS_VISUAL_SP_EFFECT_ID);
                if effect_is_active {
                    if let Some(enemy) = chr.as_subclass_mut::<EnemyIns>() {
                        enemy.remove_speffect(DISMISS_VISUAL_SP_EFFECT_ID);
                        log_line(format_args!(
                            "dismissal finish vfx: buddy={} sp_effect={} remaining={:.2} fade_alpha={:.2} removed=true",
                            group.buddy_param_id, DISMISS_VISUAL_SP_EFFECT_ID, remaining, alpha
                        ));
                    }
                }
            }
            chr.opacity_keyframes_multiplier = alpha;
            chr.opacity_keyframes_multiplier_previous = alpha;
        }
    }
    fade_backups.retain(|character_key, _| live_characters.contains(character_key));
    animation_ids.retain(|character_key, _| live_characters.contains(character_key));
}

fn current_animation(chr: &ChrIns) -> (i32, f32, f32) {
    let time_act = chr.modules.as_ref().time_act.as_ref();
    let animation_count = time_act.anim_queue.len();
    if animation_count == 0 {
        return (-1, 0.0, 0.0);
    }
    let animation_index = (time_act.read_idx % animation_count as u32) as usize;
    let animation = &time_act.anim_queue[animation_index];
    (
        animation.anim_id,
        animation.play_time,
        animation.anim_length,
    )
}

fn apply_dismissal_visual_effect(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    state: &mut RuntimeState,
) {
    let mut fading_characters = HashSet::new();
    for entry in manager.groups.iter() {
        for group in entry.second.iter() {
            if !group.disappear_requested || !chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }
            if let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied()
                && state.dismissed_effects.contains(&effect)
                && state
                    .native_exit_handoff
                    .as_ref()
                    .is_none_or(|handoff| handoff.effect != effect)
            {
                fading_characters.insert(group.chr_ins.as_ptr() as usize);
            }
        }
    }

    state
        .dismissal_visual_effects_applied
        .retain(|character| fading_characters.contains(character));
    if fading_characters.is_empty() {
        return;
    }

    let Ok(repository) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let Some(param) = repository.get::<SpEffectParam>(DISMISS_VISUAL_SP_EFFECT_ID as u32) else {
        if !DISMISS_VFX_PARAM_MISSING_REPORTED.swap(true, Ordering::AcqRel) {
            log_line(format_args!(
                "dismissal visual effect unavailable: sp_effect={} param_row_missing=true",
                DISMISS_VISUAL_SP_EFFECT_ID
            ));
        }
        return;
    };

    let effect_endurance = param.effect_endurance();
    let vfx_ids = [
        param.vfx_id(),
        param.vfx_id1(),
        param.vfx_id2(),
        param.vfx_id3(),
        param.vfx_id4(),
        param.vfx_id5(),
        param.vfx_id6(),
        param.vfx_id7(),
    ];
    if !DISMISS_VFX_PARAM_REPORTED.swap(true, Ordering::AcqRel) {
        log_line(format_args!(
            "dismissal visual effect param: sp_effect={} endurance={:.2} vfx_ids={},{},{},{},{},{},{},{}",
            DISMISS_VISUAL_SP_EFFECT_ID,
            effect_endurance,
            vfx_ids[0],
            vfx_ids[1],
            vfx_ids[2],
            vfx_ids[3],
            vfx_ids[4],
            vfx_ids[5],
            vfx_ids[6],
            vfx_ids[7]
        ));
        let mut reported_vfx_ids = HashSet::new();
        for vfx_id in vfx_ids.into_iter().filter(|id| *id >= 0) {
            if !reported_vfx_ids.insert(vfx_id) {
                continue;
            }
            if let Some(vfx) = repository.get::<SpEffectVfxParam>(vfx_id as u32) {
                log_line(format_args!(
                    "dismissal vfx row: id={} init_sfx={} midst_sfx={} finish_sfx={} init_dmy={} midst_dmy={} finish_dmy={} type={} category={} priority={}",
                    vfx_id,
                    vfx.init_sfx_id(),
                    vfx.midst_sfx_id(),
                    vfx.finish_sfx_id(),
                    vfx.init_dmy_id(),
                    vfx.midst_dmy_id(),
                    vfx.finish_dmy_id(),
                    vfx.effect_type(),
                    vfx.play_category(),
                    vfx.play_priority()
                ));
            } else {
                log_line(format_args!(
                    "dismissal vfx row unavailable: id={} param_row_missing=true",
                    vfx_id
                ));
            }
        }
    }

    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let Some(target_effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                continue;
            };
            if !state.dismissed_effects.contains(&target_effect)
                || !group.disappear_requested
                || !chr_alive(unsafe { group.chr_ins.as_ref() })
                || state
                    .native_exit_handoff
                    .as_ref()
                    .is_some_and(|handoff| handoff.effect == target_effect)
            {
                continue;
            }

            let character_key = group.chr_ins.as_ptr() as usize;
            if state
                .dismissal_visual_effects_applied
                .contains(&character_key)
            {
                continue;
            }

            let chr = unsafe { group.chr_ins.as_mut() };
            let already_active = chr
                .special_effect
                .as_ref()
                .entries()
                .any(|entry| entry.param_id == DISMISS_VISUAL_SP_EFFECT_ID);
            let applied = if already_active {
                true
            } else if let Some(enemy) = chr.as_subclass_mut::<EnemyIns>() {
                enemy.apply_speffect(DISMISS_VISUAL_SP_EFFECT_ID, true);
                true
            } else {
                false
            };

            state.dismissal_visual_effects_applied.insert(character_key);
            log_line(format_args!(
                "dismissal visual effect: target_effect={} buddy={} sp_effect={} destination=ash applied={} already_active={}",
                target_effect,
                group.buddy_param_id,
                DISMISS_VISUAL_SP_EFFECT_ID,
                applied,
                already_active
            ));
        }
    }
}

fn restore_dismissal_fade(group: &mut SummonBuddyGroup, backup: DismissalFadeBackup) {
    let chr = unsafe { group.chr_ins.as_mut() };
    chr.debug_flags
        .set_disabled_secondary_actions(backup.disabled_secondary_actions);
    chr.debug_flags
        .set_disabled_movement(backup.disabled_movement);
    chr.opacity_keyframes_multiplier = backup.opacity_keyframes_multiplier;
    chr.opacity_keyframes_multiplier_previous = backup.opacity_keyframes_multiplier_previous;
}

fn report_preserved_effect(effect: i32, source: &str) {
    let Ok(mut reported) = REPORTED_PRESERVED_EFFECTS.lock() else {
        return;
    };
    if reported.insert(effect) {
        log_line(format_args!(
            "vanilla dismissal suppressed: source={} effect={}",
            source, effect
        ));
    }
}

fn group_snapshot(manager: &SummonBuddyManager, buddy_to_trigger: &HashMap<i32, i32>) -> String {
    let mut groups = Vec::new();
    for entry in manager.groups.iter() {
        for group in entry.second.iter() {
            let chr = unsafe { group.chr_ins.as_ref() };
            let data = chr.modules.as_ref().data.as_ref();
            let effect = buddy_to_trigger
                .get(&group.buddy_param_id)
                .map_or_else(|| "?".to_owned(), i32::to_string);
            groups.push(format!(
                "buddy={}:effect={}:hp={:.0}:dismiss={}:delay={:.2}:fade_alpha={:.2}:updates_paused={}:actions_paused={}:movement_paused={}",
                group.buddy_param_id,
                effect,
                data.hp,
                group.disappear_requested,
                group.disappear_delay_sec,
                chr.opacity_keyframes_multiplier,
                chr.debug_flags.disabled_updates(),
                chr.debug_flags.disabled_secondary_actions(),
                chr.debug_flags.disabled_movement()
            ));
        }
    }
    if groups.is_empty() {
        "none".to_owned()
    } else {
        groups.join("|")
    }
}

fn summon_buddy_manager_ptr(world: &mut WorldChrMan) -> Option<*mut SummonBuddyManager> {
    // The binding models this as a non-null pointer, but the game can leave
    // the field null during save transitions. Read its address before forming
    // a Rust reference.
    let address = unsafe {
        std::ptr::addr_of!(world.summon_buddy_manager)
            .cast::<usize>()
            .read()
    };
    (address != 0).then_some(address as *mut SummonBuddyManager)
}

fn classify_item_action(
    requested_effect: i32,
    currently_living: &HashSet<i32>,
    toggle_dismiss: bool,
) -> Option<ItemAction> {
    if requested_effect <= 0 {
        return None;
    }

    if toggle_dismiss && currently_living.contains(&requested_effect) {
        Some(ItemAction::Dismiss(requested_effect))
    } else {
        Some(ItemAction::Summon(requested_effect))
    }
}

fn apply_item_action(
    manager: &mut SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    action: ItemAction,
    dismissed_effects: &mut HashSet<i32>,
) {
    let dismissed_target = match action {
        ItemAction::Summon(effect) => {
            dismissed_effects.remove(&effect);
            None
        }
        ItemAction::Dismiss(effect) => {
            dismissed_effects.insert(effect);
            Some(effect)
        }
    };

    for entry in manager.groups.iter_mut() {
        for group in entry.second.iter_mut() {
            let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id).copied() else {
                continue;
            };
            if !chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }

            if dismissed_target == Some(effect) {
                group.disappear_requested = true;
            } else if !dismissed_effects.contains(&effect) {
                // The native manager marks its previous group for removal when a
                // different item is used. Keep that group alive unless the player
                // explicitly dismissed it earlier. Reset any stale removal
                // timer along with the flag.
                group.disappear_requested = false;
                group.disappear_delay_sec = -1.0;
            }
        }
    }
}

fn buddy_trigger_map(manager: &SummonBuddyManager) -> HashMap<i32, i32> {
    let mut mapping = HashMap::with_capacity(manager.trigger_speffect_to_buddy_map.len());
    for (trigger_effect, buddy_param_id) in manager.trigger_speffect_to_buddy_map.iter() {
        if *trigger_effect > 0 && *buddy_param_id > 0 {
            mapping.entry(*buddy_param_id).or_insert(*trigger_effect);
        }
    }
    mapping
}

fn collect_live_effects(
    manager: &SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
) -> HashSet<i32> {
    collect_effects(manager, buddy_to_trigger, false)
}

fn collect_all_live_effects(
    manager: &SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
) -> HashSet<i32> {
    collect_effects(manager, buddy_to_trigger, true)
}

fn collect_effects(
    manager: &SummonBuddyManager,
    buddy_to_trigger: &HashMap<i32, i32>,
    include_dismissing: bool,
) -> HashSet<i32> {
    let mut effects = HashSet::new();
    for entry in manager.groups.iter() {
        for group in entry.second.iter() {
            if !include_dismissing && group.disappear_requested {
                continue;
            }
            if !chr_alive(unsafe { group.chr_ins.as_ref() }) {
                continue;
            }
            if let Some(effect) = buddy_to_trigger.get(&group.buddy_param_id) {
                effects.insert(*effect);
            }
        }
    }
    effects
}

fn chr_alive(chr: &ChrIns) -> bool {
    let data = chr.modules.as_ref().data.as_ref();
    data.hp > 0 && !chr.chr_flags1c5.death_flag()
}

fn chr_dead(chr: &ChrIns) -> bool {
    !chr_alive(chr)
}

#[derive(Clone, Copy)]
struct Settings {
    enabled: bool,
    follow_areas: bool,
    fast_dismissal: bool,
    low_fp_dismissal: bool,
    extra_cost_percent: u32,
    damage_reduction_percent: u32,
    max_active_ashes: usize,
    toggle_dismiss: bool,
    patch_buddy_item_limits: bool,
    suppress_spirit_ash_confirmation: bool,
    suppress_additional_summon_prompt: bool,
    clear_active_buddy_stone_link: bool,
    temporary_pending_item_unlink: bool,
    patch_hud_movie: bool,
    start_delay_ms: u64,
    log_enabled: bool,
}

impl Settings {
    /// The standalone mod's shipped INI; NativeTweaks overrides enabled,
    /// max_active_ashes and log_enabled.
    const DEFAULT: Self = Self {
        enabled: false,
        follow_areas: true,
        fast_dismissal: true,
        low_fp_dismissal: true,
        extra_cost_percent: 100,
        damage_reduction_percent: 0,
        max_active_ashes: 8,
        toggle_dismiss: true,
        patch_buddy_item_limits: true,
        suppress_spirit_ash_confirmation: false,
        suppress_additional_summon_prompt: true,
        clear_active_buddy_stone_link: false,
        temporary_pending_item_unlink: false,
        patch_hud_movie: true,
        start_delay_ms: 2500,
        log_enabled: false,
    };

    fn start_delay(self) -> Duration {
        Duration::from_millis(self.start_delay_ms)
    }
}

fn settings() -> Settings {
    SETTINGS
        .read()
        .map(|current| *current)
        .unwrap_or(Settings::DEFAULT)
}

/// The compact HUD rows need the startup HUD patch (it moves the rows and
/// hides the vanilla Ash panel); they keep running if the feature is switched
/// off later, since the vanilla panel stays hidden until the next start.
pub(crate) fn hud_rows_active() -> bool {
    HUD_ROWS_INSTALLED.load(Ordering::Acquire)
}

pub(crate) fn log_line(message: impl std::fmt::Display) {
    if !settings().log_enabled {
        return;
    }
    let Ok(mut guard) = LOG.lock() else {
        return;
    };
    if guard.is_none() {
        let name = if STANDALONE_BUILD.load(Ordering::Acquire) {
            STANDALONE_LOG_FILE_NAME
        } else {
            LOG_FILE_NAME
        };
        let Some(path) = module_dir().map(|dir| dir.join(name)) else {
            return;
        };
        *guard = OpenOptions::new().create(true).append(true).open(path).ok();
    }
    if let Some(file) = guard.as_mut() {
        let _ = writeln!(file, "{message}");
        let _ = file.flush();
    }
}

fn module_dir() -> Option<PathBuf> {
    let module = DLL_MODULE.load(Ordering::Relaxed);
    if module == 0 {
        return None;
    }

    let mut buffer = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(HMODULE(module as *mut _)), &mut buffer) };
    if len == 0 || len as usize >= buffer.len() {
        return None;
    }
    let path = PathBuf::from(OsString::from_wide(&buffer[..len as usize]));
    path.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effects(values: &[i32]) -> HashSet<i32> {
        values.iter().copied().collect()
    }

    #[test]
    fn a_new_ash_preserves_existing_ashes() {
        assert_eq!(
            classify_item_action(200, &effects(&[100]), true),
            Some(ItemAction::Summon(200))
        );
    }

    #[test]
    fn using_an_already_active_ash_dismisses_only_that_ash() {
        assert_eq!(
            classify_item_action(200, &effects(&[100, 200]), true),
            Some(ItemAction::Dismiss(200))
        );
        assert_eq!(
            classify_item_action(100, &effects(&[100, 200]), true),
            Some(ItemAction::Dismiss(100))
        );
    }

    #[test]
    fn zero_request_effect_is_not_an_item_action() {
        assert_eq!(classify_item_action(0, &effects(&[100]), true), None);
    }

    #[test]
    fn disabling_toggle_dismiss_keeps_an_active_ash() {
        assert_eq!(
            classify_item_action(100, &effects(&[100]), false),
            Some(ItemAction::Summon(100))
        );
    }

    #[test]
    fn hub_settings_override_the_shipped_defaults() {
        let default = Settings::DEFAULT;
        assert_eq!(default.max_active_ashes, 8);
        assert!(!default.enabled);
        assert!(default.patch_hud_movie);
        assert!(default.suppress_additional_summon_prompt);

        apply_hub_settings(&NtMsaSettings {
            enabled: 1,
            max_active_ashes: 3,
            log_enabled: 1,
            toggle_dismiss: 1,
            suppress_prompts: 1,
            follow_areas: 1,
            fast_dismissal: 1,
            low_fp_dismissal: 1,
            hud_rows: 1,
            extra_cost_percent: 50,
            damage_reduction_percent: 10,
        });
        let applied = settings();
        assert!(applied.enabled);
        assert_eq!(applied.max_active_ashes, 3);
        assert!(applied.log_enabled);
        assert!(applied.toggle_dismiss);
        assert!(applied.suppress_spirit_ash_confirmation);
        assert!(applied.suppress_additional_summon_prompt);

        apply_hub_settings(&NtMsaSettings {
            enabled: 0,
            max_active_ashes: 8,
            log_enabled: 0,
            toggle_dismiss: 0,
            suppress_prompts: 0,
            follow_areas: 0,
            fast_dismissal: 0,
            low_fp_dismissal: 0,
            hud_rows: 0,
            extra_cost_percent: 100,
            damage_reduction_percent: 0,
        });
        let off = settings();
        assert!(!off.enabled);
        assert!(!off.toggle_dismiss);
        assert!(!off.suppress_spirit_ash_confirmation);
        assert!(!off.suppress_additional_summon_prompt);
    }
}

#[cfg(test)]
mod layout_offsets {
    use eldenring::cs::{
        CSFeManImp, ChrIns, FrontEndViewValues, SummonBuddyGroup, SummonBuddyManager, WorldChrMan,
    };
    use std::mem::offset_of;

    /// Prints the SDK offsets this mod reads, for the offline audit
    /// (research/offset_audit.py compares their use in each executable).
    #[test]
    fn print_sdk_offsets() {
        let rows = [
            ("WorldChrMan.main_player", offset_of!(WorldChrMan, main_player)),
            ("WorldChrMan.summon_buddy_manager", offset_of!(WorldChrMan, summon_buddy_manager)),
            ("ChrIns.chr_flags1c5", offset_of!(ChrIns, chr_flags1c5)),
            ("ChrIns.debug_flags", offset_of!(ChrIns, debug_flags)),
            ("ChrIns.modules", offset_of!(ChrIns, modules)),
            ("ChrIns.team_type", offset_of!(ChrIns, team_type)),
            ("ChrIns.npc_param_id", offset_of!(ChrIns, npc_param_id)),
            ("ChrIns.field_ins_handle", offset_of!(ChrIns, field_ins_handle)),
            ("ChrIns.opacity_keyframes_multiplier", offset_of!(ChrIns, opacity_keyframes_multiplier)),
            ("ChrIns.opacity_keyframes_multiplier_previous", offset_of!(ChrIns, opacity_keyframes_multiplier_previous)),
            ("SummonBuddyManager.groups", offset_of!(SummonBuddyManager, groups)),
            ("SummonBuddyManager.active_summon_goods_id", offset_of!(SummonBuddyManager, active_summon_goods_id)),
            ("SummonBuddyManager.requested_summon_goods_id", offset_of!(SummonBuddyManager, requested_summon_goods_id)),
            ("SummonBuddyManager.disappear_requested", offset_of!(SummonBuddyManager, disappear_requested)),
            ("SummonBuddyManager.active_summon_speffect_id", offset_of!(SummonBuddyManager, active_summon_speffect_id)),
            ("SummonBuddyManager.request_summon_speffect_id", offset_of!(SummonBuddyManager, request_summon_speffect_id)),
            ("SummonBuddyManager.active_summmon_buddy_stone_entity_id", offset_of!(SummonBuddyManager, active_summmon_buddy_stone_entity_id)),
            ("SummonBuddyManager.trigger_speffect_to_buddy_map", offset_of!(SummonBuddyManager, trigger_speffect_to_buddy_map)),
            ("SummonBuddyManager.player_has_alive_summon", offset_of!(SummonBuddyManager, player_has_alive_summon)),
            ("SummonBuddyManager.item_use_cooldown_timer", offset_of!(SummonBuddyManager, item_use_cooldown_timer)),
            ("SummonBuddyManager.spawn_origin", offset_of!(SummonBuddyManager, spawn_origin)),
            ("SummonBuddyManager.is_within_activation_range", offset_of!(SummonBuddyManager, is_within_activation_range)),
            ("SummonBuddyGroup.buddy_param_id", offset_of!(SummonBuddyGroup, buddy_param_id)),
            ("SummonBuddyGroup.disappear_requested", offset_of!(SummonBuddyGroup, disappear_requested)),
            ("SummonBuddyGroup.disappear_delay_sec", offset_of!(SummonBuddyGroup, disappear_delay_sec)),
            ("CSFeManImp.front_end_view", offset_of!(CSFeManImp, front_end_view)),
            ("CSFeManImp.friendly_chr_tag_displays", offset_of!(CSFeManImp, friendly_chr_tag_displays)),
            ("CSFeManImp.boss_health_displays", offset_of!(CSFeManImp, boss_health_displays)),
            ("FrontEndViewValues.friendly_chr_tag_data", offset_of!(FrontEndViewValues, friendly_chr_tag_data)),
            ("FrontEndViewValues.boss_list_tag_data", offset_of!(FrontEndViewValues, boss_list_tag_data)),
            ("FrontEndViewValues.summoned_spirit_ash_count", offset_of!(FrontEndViewValues, summoned_spirit_ash_count)),
        ];
        for (name, offset) in rows {
            println!("OFFSET {name} {offset:#x}");
        }
    }
}
