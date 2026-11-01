# Changelog

Same entries as the [Nexus page](https://www.nexusmods.com/eldenring/mods/11158). Newest first.

## 1.0.0

- Leaves alpha: no more Alpha in the name, and the version is now 1.0.0.
- Fixed: with Enabled=false the game closed a few seconds after starting (since 0.2.5). With Enabled=true, the default, nothing changes.
- Update: replace the DLL; your INI can stay.

## 0.1.9.0

- Fixed: SummonAnywhere, KeepAshesAfterBoss and ResummonWithoutRest did nothing with Enabled=false - with several ashes off you could not summon away from a monument (Roundtable Hold, Radahn). They now work on their own (Nexus report).
- Update: replace the DLL; your INI can stay (only the comment on Enabled was reworded).

## 0.1.8.0

- New: SolidAshes (off by default) - summoned ashes look solid, like normal characters, instead of the blue ghost look (player request).
- Update: replace the DLL and add the line SolidAshes=true to your INI if you want it (or take the new INI).

## 0.1.7.0

- Fixed: Seamless Co-op no longer stops at launch with "No such pattern" while this mod is loaded; the patch that makes room for 500 spirits now leaves the game code Seamless Co-op looks for untouched (Nexus report).
- Update: replace the DLL; the INI is unchanged from Alpha 6 (keep yours).

## 0.1.6.0

- Fixed: upgraded spirit ashes (+1 to +10) work with several ashes at once - they can be summoned beside others, and their item dismisses only that ash (Nexus report).
- Fixed: Summon anywhere works in every area of the game and the DLC, boss arenas included (Radahn's battlefield, Nexus report); ashes summoned away from a monument no longer stand frozen, and a refused summon no longer comes out later in place of the ash you chose.
- Fixed: after all your ashes die you can summon again at once (before, summoning stayed blocked until you rested).
- Fixed: summons past 10 spirits no longer spend the FP for nothing (the game keeps 10 spirits per player; Lone Wolf Ashes gave 1 wolf of 3).
- Changed: up to 500 spirits at once and no ash limit by default (MaxActiveAshes=0); the HUD shows 7 ash rows.
- New: RepeatSameAsh (off by default) - using a summoned ash's item summons it once more instead of dismissing it.
- Update: replace the DLL and the INI (MaxActiveAshes, SummonAnywhere and the new RepeatSameAsh changed).

## 0.1.5.9

- Fixed: summoning a new ash right after the current one died no longer dismisses every other living ash (found in the Godrick fight).
- Fixed: summoning the same ash again after it died no longer dismisses the other living ashes either.
- Fixed: using an ash's item could be taken as dismissing another living ash, so the summoning animation played but nobody came; items are now matched to their ash by the game's own item data.
- Fixed: fallen ashes are released two seconds after they die; they used to pile up until no new summon worked.
- Update: replace the DLL; the INI is unchanged from Alpha 4 (keep yours).
- Fixed: summoning a new ash right after the current one died no longer dismisses every other living ash (found in the Godrick fight).
- Fixed: summoning the same ash again after it died no longer dismisses the other living ashes either.
- Fixed: using an ash's item could be taken as dismissing another living ash, so the summoning animation played but nobody came; items are now matched to their ash by the game's own item data.
- Fixed: fallen ashes are released two seconds after they die; they used to pile up until no new summon worked.
- Update: replace the DLL; the INI is unchanged from Alpha 4 (keep yours).

## 0.1.5.8

- Several ashes at once is complete: different spirit ashes stay together up to MaxActiveAshes (2-20), each paying its FP (before, additional ashes were free).
- No confirmation boxes when summoning another ash or dismissing one (the game's own prompt check is handled; the old per-item text trick could still show it).
- Using a summoned ash's item again dismisses only that one with a short fade; it works even when your FP is below its cost, and the ash can be summoned again at the first try.
- Ashes follow you into new areas and through doors, fog walls and lifts (the game's own warp with fade), and an ash left behind while you travel is brought back.
- Summoning monuments (on by default, they work together): SummonAnywhere (no dismissal for walking away from the monument), KeepAshesAfterBoss, ResummonWithoutRest.
- Balance options: AshCostPercent, ExtraAshCostPercent, DamageLossPerExtraAshPercent. Optional DismissAllKey.
- HUD: one compact row per ash with its real name and HP (up to 6; "xN" for groups, "+N" when there are more). The rows now really show (the game copies the co-op rows every frame; they are written in that copy).
- Compatibility download (HudRows=false): the HUD is left alone, for Seamless Co-op and other HUD mods.
- Elden Ring 1.17.2 Japanese (eldenring.exe 2.7.2.1) supported, besides 1.17.2 (2.7.2.0) and 1.17.1 (2.7.1.0).
- The INI keys changed: replace the DLL and the INI when updating from Alpha 3 or older.
- NativeTweaks r135 includes this mod; use one or the other (if this DLL is loaded, the NativeTweaks copy stays off).

## 0.1.5.7

- 0.1.56 Alpha 3: support for Elden Ring 1.17.2 (eldenring.exe 2.7.2.0); 1.17.1 (2.7.1.0) keeps working with the same DLL.
- The package no longer contains menu/01_000_fe.gfx. The DLL applies its HUD changes in memory when the game loads the HUD, on top of whichever 01_000_fe.gfx is loaded, so it no longer conflicts with mods that ship their own HUD file.
- If the loaded HUD file lacks an element a change needs, that change is skipped and the HUD is left as loaded. New INI option PatchHudMovie (default true).
- If you installed Alpha 1, remove its menu/01_000_fe.gfx (reinstalling the mod in MO2 does this).
- The native hooks are checked against their verified signatures for the detected executable; other versions are left untouched. No other feature or setting changes.
