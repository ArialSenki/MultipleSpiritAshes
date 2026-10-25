use pelite::pe64::PeView;
use std::sync::LazyLock;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::core::PCSTR;

mod bundle;
mod rva_jp;
mod rva_jp2721;
mod rva_ww;
mod rva_ww2720;

pub use bundle::RvaBundle;

use fromsoftware_shared::game_version::{GameVersion, LANG_ID_EN, LANG_ID_JP};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ERGameVersion {
    Ww2700,
    Ww2710,
    Ww2720,
    Jp2701,
    Jp2721,
}

impl GameVersion for ERGameVersion {
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

impl ERGameVersion {
    const fn rvas(self) -> RvaBundle {
        match self {
            Self::Ww2700 => rva_ww::RVAS,
            Self::Ww2710 => RvaBundle {
                register_task: 0xeb3e50,
                cs_phys_world_cast_ray: 0xc71e00,
                cs_ez_draw_draw_line: 0x2652640,
                cs_ez_draw_draw_triangle: 0x2652390,
                cs_ez_draw_draw_wedge: 0x2651f40,
                ..rva_ww::RVAS
            },
            Self::Ww2720 => rva_ww2720::RVAS,
            Self::Jp2701 => rva_jp::RVAS,
            Self::Jp2721 => rva_jp2721::RVAS,
        }
    }
}

/// Returns the RVA bundle for the current executable region and version.
///
/// This will panic if the current executable isn't supported by this package.
pub fn get() -> &'static RvaBundle {
    static RVAS: LazyLock<RvaBundle> = LazyLock::new(|| {
        let module = unsafe {
            PeView::module(GetModuleHandleA(PCSTR(std::ptr::null())).unwrap().0 as *const u8)
        };
        ERGameVersion::detect(&module)
            .unwrap_or_else(|e| panic!("{e}"))
            .rvas()
    });

    &RVAS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_verified_1171_without_changing_1170() {
        let current = ERGameVersion::from_lang_version(LANG_ID_EN, "2.7.1.0")
            .unwrap()
            .rvas();
        let previous = ERGameVersion::from_lang_version(LANG_ID_EN, "2.7.0.0")
            .unwrap()
            .rvas();
        assert_eq!(current.register_task, 0xeb3e50);
        assert_eq!(previous.register_task, 0xeb3de0);
        assert_eq!(current.cs_phys_world_cast_ray, 0xc71e00);
        assert_eq!(current.cs_ez_draw_draw_line, 0x2652640);
        assert_eq!(current.cs_ez_draw_draw_triangle, 0x2652390);
        assert_eq!(current.cs_ez_draw_draw_wedge, 0x2651f40);
        assert_eq!(
            current.chr_ins_apply_speffect,
            previous.chr_ins_apply_speffect
        );
        assert_eq!(
            current.chr_ins_remove_speffect,
            previous.chr_ins_remove_speffect
        );
        assert_eq!(
            ERGameVersion::from_lang_version(LANG_ID_JP, "2.7.1.1"),
            None
        );
        assert_eq!(
            ERGameVersion::from_lang_version(LANG_ID_EN, "2.8.0.0"),
            None
        );
        let ww2720 = ERGameVersion::from_lang_version(LANG_ID_EN, "2.7.2.0")
            .unwrap()
            .rvas();
        assert_eq!(ww2720.world_chr_man_dbg_flags, 0x3d6d230);
        assert_eq!(ww2720.solo_param_repository_vmt, 0x2bba778);
        let jp2721 = ERGameVersion::from_lang_version(LANG_ID_JP, "2.7.2.1")
            .unwrap()
            .rvas();
        // Same code as WW 2.7.2.0; only some vtables moved.
        assert_eq!(jp2721.register_task, ww2720.register_task);
        assert_eq!(jp2721.chr_ins_apply_speffect, ww2720.chr_ins_apply_speffect);
        assert_ne!(jp2721.csez_task_vmt, ww2720.csez_task_vmt);
    }
}
