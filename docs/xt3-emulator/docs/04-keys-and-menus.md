# M4 — Keys and menus

## Where keys enter the firmware

The FujiMod catalog for the X‑T3 already has:

- the scan table (136 rows at `0x14fc018`);
- the physical key → event translation (`0x13d0f08`);
- 263 debug commands.

MENU/OK, the cross keys and DISP/BACK are a resistor ladder on ADC source 3. Their thresholds come from a
calibration table that is erased in our flash, so an "electrical" press would need the real body's
calibration data.

Instead, keys are injected into the firmware's **key queue**:

- Entries are 2 bytes (physical key, state) at `0x15634fc`, with a u32 count at `0x156360c`.
- `0x10c53b4` and `0x10c5684` fill the queue.
- `0x10c6128` drains it, calling `0x10c606c(key, state)` with 1 = press and 0 = release, then resets the
  count.

`xp4_run` writes the entry and the count between CPU slices, with all cores stopped.

| Key | Physical code |
|---|---|
| MENU/OK | 22 |
| Up / Down / Left / Right | 26 / 27 / 28 / 29 |
| DISP/BACK | 23 |
| Playback | 11 |

Timing: the UI takes 100–200 M instructions to react inside menus (> 15 M on the first‑start pages). A capture
often shows the effect of the *previous* press. Presses around 1.76 G are ignored, because live view resets
itself then.

## Menus (strict)

- The 1st MENU/OK press (2.1 G) closes the card warning and shows the AF frame.
- The 2nd press (2.25 G) opens the **IMAGE QUALITY SETTING** menu, page 1/3, in French: image size L 3:2,
  quality F, uncompressed RAW, film simulation STD, white balance AUTO, 6240×4160 (26M).
- Navigation checked in 4 strict runs, up to 3.2 G instructions, with no wall:
  - the IMAGE SIZE sub‑menu opens (L/M/S × 3:2, 16:9, 1:1);
  - the cursor moves;
  - the displayed resolution updates: 4160×4160 (17M) for L 1:1, 6240×3512 (22M) for L 16:9.
- Playback without a card stays on the shooting screen ("NO CARD").

## Menu navigation facts

- RIGHT enters a sub‑menu.
- UP from the first item of a menu goes back to the last item of the previous menu, across tabs. From
  IMAGE QUALITY SETTING item 1, UP lands on SET UP (wrench) → CONNECTION SETTING.
- LEFT from IMAGE QUALITY SETTING item 1 does nothing.
- SET UP lists: USER SETTING, SOUND SET‑UP, SCREEN SET‑UP, BUTTON/DIAL SETTING, POWER MANAGEMENT,
  SAVE DATA SET‑UP, CONNECTION SETTING.
- Path to BUTTON/DIAL SETTING: MENU, UP ×4, RIGHT. Its page 2/3 has SHOOT WITHOUT LENS (default OFF) and
  SHOOT WITHOUT CARD (default ON).
- Space presses at least 100 M instructions apart, and confirm each step with a capture.

| Other keys (physical code) | |
|---|---|
| Shutter half press S1 / full press S2 | 2 / 3 |
| Movie record | 190 |
| Delete | 24 |
| Q | 25 |
| Focus mode selector M / C / S (software rows) | 47 / 46 / 45 |
