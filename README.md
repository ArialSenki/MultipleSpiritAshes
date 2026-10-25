# Multiple Spirit Ashes

An Elden Ring mod that lets you summon several different spirit ashes at the
same time and dismiss them one by one. Download and player instructions are on
[Nexus Mods](https://www.nexusmods.com/eldenring/mods/11158); this repository
has the source.

The same code ships in two ways:

- **`MultipleSpiritAshes.dll`** (the standalone mod, built from `standalone/`),
  configured by `MultipleSpiritAshes.ini` next to the DLL. Its player README
  is [standalone/README.md](standalone/README.md).
- **Inside [NativeTweaks](https://github.com/ArialSenki/NativeTweaks)**, as a
  static library that NativeTweaks links in and configures from its in-game
  menu.

Supported game versions: Elden Ring 1.17.2 (eldenring.exe 2.7.2.0 and the
Japanese 2.7.2.1) and 1.17.1 (2.7.1.0), PC. On any other version the DLL
leaves the game untouched. Offline only, with Easy Anti-Cheat disabled.

## How it works, briefly

- `src/lib.rs` hooks the summon and dismiss paths so a new ash doesn't replace
  the ones already out, and keeps track of each ash's FP cost.
- `src/area_follow.rs` brings the ashes along through doors, fog walls, lifts
  and area changes using the game's own warp.
- `src/hud.rs` and `src/hud_movie.rs` replace the single spirit panel with one
  row per ash. The HUD movie is patched in memory, so no game files are shipped.
- `src/native_prompt.rs` skips the summon/replace confirmation boxes.

Every hook checks the game version and the bytes it is about to change before
touching anything.

## Building

Requirements: Rust (stable, `x86_64-pc-windows-msvc` target) and the MSVC
2022 build tools.

```bat
build.cmd standalone        rem MultipleSpiritAshes.dll
build.cmd                   rem static library used by NativeTweaks
build.cmd test              rem unit tests
```

The output goes to `%LOCALAPPDATA%\MultipleSpiritAshes-standalone-build` (and
`NativeTweaks-msa-build` for the library). See the top of `build.cmd` for the
environment variables it accepts.

Some HUD tests need game files that can't be redistributed. Copy the
`01_000_fe.gfx` movies into `test-gfx\` as described in `src/hud_movie.rs`, or
those tests are skipped.

## Third-party code

`vendor/fromsoftware-rs` is a copy of
[fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs) by Vincent
Swarte and contributors, with address tables added for 2.7.2.0 and 2.7.2.1.
It keeps its own MIT / Apache-2.0 licenses. The rest of the dependencies come
from crates.io; see [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt).

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.

Elden Ring is a trademark of FromSoftware / Bandai Namco. This is a fan-made
mod, not affiliated with them.
