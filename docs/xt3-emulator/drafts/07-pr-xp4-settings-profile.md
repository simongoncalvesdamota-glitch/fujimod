# PR draft — XP4: boot X‑T3 5.11 as a configured body (settings profile applied in the emulator)

**Title:** emulator‑xp4: apply the X‑T3 settings profile (power‑controller header + NOR journal), more profile settings

---

Hi! This builds on #184 (heblushabus), which added the X‑T3 5.11 RAM journal profile (header at `0x18d28b0`,
main record at `0x168e20c`) and the `setup`, `language` and `debug` settings. #184 declares
`SettingsProfile.ram_address` but nothing in the emulator applies an XP4 profile yet. We traced where X‑T3 5.11
keeps both parts across a power cycle, and checked it in the XP4 board emulator. With the profile applied, the
camera boots straight to the shooting screen with no key press, in about 0.9 G instructions. Without it, the
setup screens and menu presses took about 10 G.

## Where the profile lives

**Header (setup state): power‑controller registers 48–55, not NOR.**
- `0x1124598` recomputes the header checksum (byte 15, so that the 16 bytes sum to zero).
- It then calls `0x10a33e8`, which writes the header as eight half‑words to power‑serial registers `0x30…0x37`
  through `0x10a20d4`.
- After the setup screens, the registers equal the RAM header bit for bit.
- Byte 12 holds the setup bits. Accessors: `0x1125684`/`0x1125710` (bit 0), `0x1125734`/`0x11257c0` (bit 1).
- A NOR image that already holds the settings, booted with registers 48–55 at 0, falls back to the stock header
  and opens the language screen. So does a fresh camera.

**Clock.** The power‑controller RTC must run (register 32 = `0x8000`). Otherwise the camera skips the language
screen but stops on the area and date screens.

**Journal: two sealed NOR copies at `0x3fa0000` and `0x3fa8000` (32 KiB each).**
- They are formatted at first boot, as are markers at `0x3f80000` and small state records at `0x3e00000`.
- `fujimod_settings::seal_journal` with `parity_offset` 31740 validates both copies.
- Writer `0x11244b4`:
  - record `[4, 0x104)` ← `0x18da394`;
  - record `[0x104, 0x104 + 0x7ad4)` ← `0x18d28c0`, the bytes after the RAM header;
  - version `*(0x14798f6/7)` → record `+0x7bf8/+0x7bf9`;
  - record `[0, 4)` holds the copy number.
- The live settings set at `0x181c3a0` (stride `0x83d8`, 7 sets) equals `0x18d28b0` from offset `0x10`. So for a
  setting at live offset `k`, the main‑record offset is `k + 0xf4`. For example, `UI_SETP_LANGUAGE` is at live +469,
  which is record +713, the offset in #184.
- The RAM journal does not change while the camera runs, even after menu changes. It is rewritten later,
  presumably at power‑off.

**Generic setting reader:** `0xf210b8` → `0xff55ac` → `0xfe7988`, a jump table on the `UI_SETP_*` id. The
`UI_SETP_*` names come from the compare chain at `0xf337c4…`. The allowed values per id are in the table of
16‑byte rows pointed to from `0x14e5560` (converter `0xff8288`).

## Settings to add to the X‑T3 profile (main record offsets)

| Setting | `UI_SETP_*` (id) | Live offset | Record offset | Values | Checked |
|---|---|---|---|---|---|
| shoot without lens | `UNMOUNT_SHUTTER` (236) | `0x2f7` | 1003 | on = 0, off = 1 (factory) | memory after boot |
| shoot without card | `NOCARD_SHUTTER` (237) | `0x2f8` | 1004 | factory 0 | — |
| shutter type | `SHUTTER_TYPE` (0x452) | `0x436` | 1258 | MS = 0 (factory), ES = 1 | "ES" on screen |
| image size | `PIXEL` (0x2d5) | 656 | 900 | L = 0 (factory), S = 2 | "S" on screen, 3153 shots |
| image quality | `QUALITY` (0x2da) | 658 | 902 | FINE = 0 (factory), NORMAL = 1 | "N" on screen |
| language | `LANGUAGE` (0x58) | 469 | 713 | en = 1 (#184), fr = 3 | French UI |
| movie size | `MOVIE_PIXEL` (0x2a6) | 659 | 903 | factory 0; FHD = 2 (name registry) | not yet |
| movie frame rate | `MOVIE_FPS` (0x2a8) | 662 | 906 | factory 3 (29.97p); 24p = 1 | not yet |
| movie bit rate | `MOVIE_BITRATE` (0x2ab) | 664 | 908 | factory 2 (100); 50 Mbit/s = 3 | not yet |
| movie codec | `MOVIE_CODEC` (0x2ac) | 665 | 909 | H.264 = 1 (factory) | not yet |

`UI_SETP_LENS_SW` (1448) is not the "shoot without lens" setting.

## Proposed changes

1. `emulator-xp4-io/src/power_serial.rs`: accept optional inputs for registers 48–55. They are read back until
   the firmware writes them. Default unchanged. Test:
   `a_declared_settings_header_reads_back_until_the_firmware_rewrites_it`.
2. Applying an XP4 profile in a run:
   - header → power inputs 48–55: the stock header from the #184 `profile.header` range, byte 12 = `0x1e`, checksum
     recomputed;
   - RTC running;
   - main record → both NOR journal copies, sealed.
3. Add the settings above to the X‑T3 profile through the settings generator (`.generated` files are not edited
   by hand).

## Evidence (emulator, X‑T3 5.11, no key press)

- Applying the profile with the language set to French, shoot without lens on and ES gives, at 0.9 G, the shooting
  screen in French with "ES". The live settings after boot read language 3, `UNMOUNT_SHUTTER` 0,
  `SHUTTER_TYPE` 1, UI mode REC.
- Adding image size S and NORMAL quality gives "S N" and 3153 shots left (545 at the factory L FINE).

<sub>Placeholders: none. No firmware bytes: addresses, offsets and values only.</sub>
