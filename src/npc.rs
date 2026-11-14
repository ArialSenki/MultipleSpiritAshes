//! NPC Spawner (the author's mod, `NPCTestArena\mod`), built into
//! NativeTweaks.dll with the "npc-spawner" feature. The Hub starts it early
//! (before ERNativeUI closes registration: it adds its own Mod Settings page
//! and F11 row) only when its option is on and NpcSpawner.dll is not there.
//! Without the feature (the standalone MultipleSpiritAshes.dll), both
//! functions do nothing, so NativeTweaks links the same either way.

#[cfg(feature = "npc-spawner")]
fn folder(dir: *const u16, len: usize) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if dir.is_null() || len == 0 || len > 32_768 {
        return None;
    }
    // SAFETY: the Hub passes a UTF-16 buffer of `len` units that outlives the call.
    let wide = unsafe { std::slice::from_raw_parts(dir, len) };
    Some(std::ffi::OsString::from_wide(wide).into())
}

/// 1 when NPC Spawner is built into this library.
#[unsafe(no_mangle)]
pub extern "C" fn nt_npc_spawner_built_in() -> u8 {
    u8::from(cfg!(feature = "npc-spawner"))
}

/// 1 when the standalone NpcSpawner.dll is loaded or next to NativeTweaks
/// (`dir`, UTF-16, `len` units): then the built-in copy must stay off.
#[unsafe(no_mangle)]
pub extern "C" fn nt_npc_spawner_standalone(dir: *const u16, len: usize) -> u8 {
    #[cfg(feature = "npc-spawner")]
    {
        let Some(dir) = folder(dir, len) else { return 0 };
        // A panic counts as "present": the built-in copy then stays off.
        u8::from(std::panic::catch_unwind(|| npc_spawner::standalone_present(&dir)).unwrap_or(true))
    }
    #[cfg(not(feature = "npc-spawner"))]
    {
        let _ = (dir, len);
        0
    }
}

/// Starts the built-in NPC Spawner once without a provider of its own: its
/// pages go into NativeTweaks' menu through nt_npc_spawner_add_pages (its F11
/// row stays). Same files as nt_npc_spawner_start; 1 when started.
#[unsafe(no_mangle)]
pub extern "C" fn nt_npc_spawner_start_hosted(dir: *const u16, len: usize, module: usize) -> u8 {
    #[cfg(feature = "npc-spawner")]
    {
        let Some(dir) = folder(dir, len) else { return 0 };
        u8::from(std::panic::catch_unwind(|| npc_spawner::start_builtin_hosted(&dir, module)).is_ok())
    }
    #[cfg(not(feature = "npc-spawner"))]
    {
        let _ = (dir, len, module);
        0
    }
}

/// From NativeTweaks' menu registration, after Vanilla Settings and before
/// its commit: an "NPC Spawner" submenu with its sections in `parent_page`
/// of `provider` (ERUI handles). 1 when every row was added.
#[unsafe(no_mangle)]
pub extern "C" fn nt_npc_spawner_add_pages(provider: u64, parent_page: u64) -> u8 {
    #[cfg(feature = "npc-spawner")]
    {
        u8::from(std::panic::catch_unwind(|| npc_spawner::add_pages(provider, parent_page)).unwrap_or(false))
    }
    #[cfg(not(feature = "npc-spawner"))]
    {
        let _ = (provider, parent_page);
        0
    }
}

/// Starts the built-in NPC Spawner once (its own threads, never blocks):
/// NpcSpawner.ini and NpcSpawner.log in `dir`. `module` is NativeTweaks.dll's
/// HMODULE. 1 when started.
#[unsafe(no_mangle)]
pub extern "C" fn nt_npc_spawner_start(dir: *const u16, len: usize, module: usize) -> u8 {
    #[cfg(feature = "npc-spawner")]
    {
        let Some(dir) = folder(dir, len) else { return 0 };
        u8::from(std::panic::catch_unwind(|| npc_spawner::start_builtin(&dir, module)).is_ok())
    }
    #[cfg(not(feature = "npc-spawner"))]
    {
        let _ = (dir, len, module);
        0
    }
}
