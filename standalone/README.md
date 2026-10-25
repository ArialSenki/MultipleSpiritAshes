# Multiple Spirit Ashes 0.2.1

For Elden Ring (Windows): **1.17.2** (eldenring.exe 2.7.2.0, and the Japanese 2.7.2.1) and **1.17.1** (2.7.1.0). One DLL for all of them. Play offline with Easy Anti-Cheat disabled.

## What it does

- **Several spirit ashes at once.** Using a different spirit ash keeps the ones already summoned, up to `MaxActiveAshes` (2-20). Each ash pays its FP.
- **Dismiss one ash.** Using a summoned ash's item again dismisses only that one, with a short fade, even when your FP is below its cost.
- **No confirmation boxes** when you summon another ash or dismiss one.
- **Ashes follow you.** When you cross into another area or go through a door, fog wall or lift, the game brings your ashes over with its own fade, and an ash left far behind while you travel is brought back.
- **Summoning monuments** (on by default, they work together): summon anywhere in the monument's area without the ashes being dismissed for walking away, ashes stay after the boss of that area falls, and a fallen ash can be summoned again without resting.
- **Balance:** cost of every ash, cost of each extra ash, and damage lost per extra ash.
- **HUD:** one compact row per ash with its name and HP (up to 6; "xN" for groups, "+N" when there are more), in place of the single spirit panel.
- **Dismiss all:** an optional key dismisses every summoned ash.

Everything is set in `MultipleSpiritAshes.ini` (comments explain each line). Changes apply when the game starts; the monument rules apply after the area reloads (rest at a grace).

## Two downloads

- **Main file:** HUD rows on (`HudRows=true`).
- **Compatibility file:** `HudRows=false`. The HUD and the co-op rows are left untouched and the game's own spirit panel stays. Use it with Seamless Co-op or with other mods that change the HUD.

The two files differ only in that line.

## Installation

The archive contains `mod/external_dlls/MultipleSpiritAshes.dll` and `mod/external_dlls/MultipleSpiritAshes.ini`. Keep the two together (the DLL reads the INI and, with `LogEnabled=true`, writes `MultipleSpiritAshes.log` next to itself) and load only one copy.

NativeTweaks (r135 and later) already includes this mod (Vanilla Settings > Spirit Ashes). Use one or the other: if this DLL is loaded, the copy inside NativeTweaks stays off.

### Mod Organizer 2 (the author's setup)

- With the Elden Ring MO2 Support plugin (and its Archive Layout Installer), install the archive through MO2 and choose "Use standard MO2 layout" when asked. Enable the mod and launch the game from MO2.
- The log is written to MO2's `overwrite\DLLs\MultipleSpiritAshes.log`.

### Mod Engine 3 (not tested)

Extract the archive next to your `.me3` profile, for example into a `MultipleSpiritAshes` folder, and add it to the profile (paths are relative to the `.me3` file), keeping your other entries:

```toml
[[natives]]
path = "MultipleSpiritAshes/mod/external_dlls/MultipleSpiritAshes.dll"
```

### Mod Engine 2 (not tested)

Copy `MultipleSpiritAshes.dll` and `MultipleSpiritAshes.ini` into `<Mod Engine 2>\mod\DLLs\` and add the DLL to `external_dlls` in `config_eldenring.toml` (`[modengine]` section), keeping the DLLs you already load:

```toml
external_dlls = [
  "mod\\DLLs\\MultipleSpiritAshes.dll"
]
```

### Elden Mod Loader (not tested)

With Elden Mod Loader in `ELDEN RING\Game` (`dinput8.dll` and `mod_loader_config.ini`), copy `MultipleSpiritAshes.dll` and `MultipleSpiritAshes.ini` into `Game\mods\`. Set `load_delay` in `mod_loader_config.ini` to a low value (e.g. `0`): the HUD rows are prepared when the game first loads its HUD.

## Changes in 0.2.1

Fixes from long boss fights (Godrick):

- Summoning a new ash right after the current one died no longer dismisses every other living ash, and neither does summoning the same ash again after it died.
- Using an ash's item could be taken as dismissing another living ash, so the summoning animation played but nobody came. Items are now matched to their ash by the game's own item data.
- Fallen ashes are released two seconds after they die; they used to pile up until no new summon worked.

## Notes

- The DLL checks the game version and the code it changes before touching anything; on other versions it does nothing.
- No game files are shipped: the HUD changes are applied in memory, on top of whichever HUD file is loaded.
- Updating from 0.1.56: replace the DLL and the INI (the INI keys changed).

## Credits

Uses the fromsoftware-rs Elden Ring bindings by Vincent Swarte and contributors (with address tables for 1.17.2 and the Japanese 1.17.2 added for this release), pelite and Microsoft's windows-rs. Licenses in `THIRD-PARTY-NOTICES.txt`.
