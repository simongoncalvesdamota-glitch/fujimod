# Issue draft — X‑T3 5.11 startup card: the preloaded reader at 0x1a0000 is wiped by the kernel

**Title:** recovery: XP4 startup card for X‑T3 5.11 crashes — the kernel clears the preloaded reader at `0x1a0000`

---

Hi! Before trying the `startup` card (`ffinit.bin` + `ffboot.bin` + `ffdata.bin`) on a real X‑T3 running 5.11,
we ran it in the XP4 board emulator. It does not boot. We have not tried it on a camera.

**Setup**

- Card built with `fujimod-recovery startup xt3-511 FWUP0019.DAT <dir>` from the official 5.11 update.
- The emulator has no boot ROM, so `ffinit.bin` (which uses the ROM file ABI) is skipped, as in the X‑T20 card
  boot (#111). `ffboot.bin` is preloaded at RAM 0, where `ffinit.bin` places it after its length and CRC‑32
  checks. `ffdata.bin` is on the virtual FAT32 card.

**What happens**

1. `ffboot.bin` holds the preloaded reader (0x678 bytes) at `0x1a0000`. The init call at `0x1147c8c` is
   redirected to `0x1a03d8` (`preloaded_initialize_configuration`).
2. About 7.8 M instructions after reset, the X‑T3 kernel initializes five 128 KiB buffers. The routine at
   `0x56488`, called from `0x56598`…`0x565cc`, writes a `0x1234` header and then zero‑fills each buffer:
   - `0x124000`;
   - `0x144004`;
   - `0x164008`;
   - `0x18400c`;
   - `0x1a4010`.

   The fourth buffer spans `0x18400c`–`0x1a3fcc`, so its zero fill (the loop at `0x56504`) erases the reader.
3. About 41 M instructions after reset, the redirected call executes zeros from `0x1a03d8` up to `0x1a400c`,
   then aborts. Core 0 stays in the abort handler.

The recipe checks that the reservation is empty in the image and outside the declared BSS ranges. These buffers
pass both checks because they are filled at run time.

**Not affected:** the recovery card (`build`). It keeps the stock call (`0x111f050`) and has no reader at
`0x1a0000`. In the emulator, it reaches the official update screen with DISP/BACK held.

**Suggestion:** for X‑T3 5.11, place the reader outside `0x124000`–`0x1c4014`, or check the reservation
against these run‑time buffers. The same `0x1a0000` worked on X‑S10, X100V and X‑Pro3, so the free area seems to
differ per body.

**What we tried locally (patch available, can become a PR):**
- `Preload.program_address`, an optional per‑body address. The compiled program has exactly 13 literal words
  that hold its own addresses, at fixed offsets. They are checked, then relocated, and every hook follows.
- The program reservation is now also checked against `exclusions`, which before only guarded the startup
  defaults.
- xt3‑511 recipe:
  - an exclusion for the five kernel buffers (`0x124000` + `0xa0014`);
  - `program_address = 0xdd2000`. This area had no CPU access and was still zero after boot, setup and playback
    (memory watch over the whole run, plus a final dump).
- The rebuilt `ffboot.bin` boots in the emulator: setup screens and photo playback are bit‑identical to a NOR
  boot, and `ffdata.bin` is read through the reader. `ffinit.bin` changes only in its expected CRC‑32.
- Not yet tried on a camera.

Evidence: bisection on the emulated boot (first instruction where the reader bytes change; last clean
instruction before the abort), with the guest call trace. No firmware bytes are included here.
