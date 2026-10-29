//! Standalone Multiple Spirit Ashes: solid spirit ashes (INI SolidAshes, the
//! Hub's Solid spirit ashes). Summoned ashes carry SpEffect 295000 ([Spirit
//! Summon] Color, puppets 295200), whose SpEffectVfxParam row 57000 (57100)
//! overrides the character's look with PhantomParam 200 (201): the blue ghost
//! shading. phantomParamOverwriteId -1, as on rows without an override, leaves
//! them with their normal look. NativeTweaks.dll does the same from its Hub.

use super::*;

const ASH_GHOST_VFX: [u32; 2] = [57000, 57100];
const RETRY: Duration = Duration::from_millis(500);
const GIVE_UP_AFTER: Duration = Duration::from_secs(300);

static STARTED: Once = Once::new();

/// Called once by the standalone DLL after nt_msa_start when SolidAshes is on.
/// The rows are written as soon as the game's params are loaded.
#[unsafe(no_mangle)]
pub extern "C" fn nt_msa_set_solid_ashes(enabled: u8) {
    if enabled == 0 {
        return;
    }
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            let start = Instant::now();
            while start.elapsed() < GIVE_UP_AFTER {
                if let Ok(Some(changed)) = std::panic::catch_unwind(write_rows) {
                    log_line(format_args!("solid ashes: {changed} ghost-look SpEffectVfxParam rows cleared"));
                    return;
                }
                std::thread::sleep(RETRY);
            }
            log_line("solid ashes: SpEffectVfxParam not found, ashes keep their ghost look");
        });
    });
}

/// None while the params aren't loaded yet.
fn write_rows() -> Option<usize> {
    let repository = unsafe { SoloParamRepository::instance_mut() }.ok()?;
    let mut found = false;
    let mut changed = 0;
    for id in ASH_GHOST_VFX {
        let Some(row) = repository.get_mut::<SpEffectVfxParam>(id) else {
            continue;
        };
        found = true;
        if row.phantom_param_overwrite_id() > 0 {
            row.set_phantom_param_overwrite_id(-1);
            changed += 1;
        }
    }
    found.then_some(changed)
}
