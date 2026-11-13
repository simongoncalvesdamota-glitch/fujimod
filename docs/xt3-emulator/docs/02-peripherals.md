# M2 — Peripherals

## Method

- **Strict run‑to‑wall** (`murs.py`): run strict until the first unmodeled access (a "wall"). Then either
  extend a FujiMod model, or declare a register file (`xpp_register_blocks`) for the free part of that page,
  with the evidence written next to the block.
- **Discovery evidence** (`preuves.py`): in discovery mode, an intervention is "kept" when the probed value
  really released a firmware wait. Kept interventions become `idle` bits or `commands` laws of a block.
- Discovery mode **does not probe pages already declared as register files**. So discovery runs go
  *without* blocks, and strict runs *with* them.
- Never declare a block inside RAM (below 1 GiB) or inside the statistics SRAM (`0xfff10000–0xfff1e000`),
  which the imaging model owns.

Result: **66 register blocks**, each with its evidence. The strict run reaches **3.2 G instructions with no
unmodeled access**, through the first‑start pages, the shooting screen and the menus.

## Register‑block model (`emulator-xpp/src/blocks.rs`)

- A block base only needs to be word‑aligned, and the block must fit in one page. It no longer has to be
  page‑aligned. Needed to start a block after another device window (e.g. `0xfff4a520`, after the DMA mux).
- 8/16‑bit accesses act on the word lane, as a read‑modify‑write. *Same idea as upstream #184.*
- A command law may target its own word: a self‑clearing "busy" bit (the job is done at once).

## Power controller (SPI1, `PowerSerial`)

- **Register 16 = power‑on reason.** Without it, the application powers itself off (`0x1070d04`, then
  `b .` at `0x1147dcc`). Chain of evidence:
  1. loader `0x15ec` reads register 16 into `0xf6e0+0x140`;
  2. the kernel copies it to `0x843d0+0x140`;
  3. the application (`0x1147750`) requires bit 2.

  The catalog declared "cold boot reason = 4" in the hand‑off block, which this loader does not read.
  **Fix: register 16 = 4.**
- **Register 4 = battery voltage** (10 bits), monitored by `0x10a1d90`. The threshold is firmware parameter
  `0x3eb`, read by `0x111ed04`: **752**. The catalog value 0 showed "battery empty" after the first key press,
  so it is now declared as **900**.
- Registers read by the firmware: 0, 2, 3, 4, 5, 6, 13, 16, 27, 35, 38. We enumerated the
  `mov r0,#n ; bl 0x10a1d40` call sites. 13 and 27 are now accepted as read‑only inputs, declared as 0 (to
  measure on hardware).
- Registers written: 2, 13, 14, 16, 17, 18, 32, 34, 35, 37, 38 (call sites of `0x10a20d4`). A written
  register reads back its last write.

## Memory

- **RAM = 1 GiB, not 512 MiB as in the catalog.** The kernel MMU table (TTB `0x120000`, written by `0x58ae4` on
  all 4 cores) identity‑maps `0x00000000–0x3fffffff` as cacheable normal memory. The application clears
  `0x3e1b6b04–0x3e1c3d30`, and unit 6 uses it. The display only comes up with 1 GiB. To confirm on hardware.
- **Memory controller, 4 channels**: base table at `0x13eee58` (`0xff600000…0xff630000`,
  `0xff500000…0xff530000`), table‑driven driver `0x108f588`:
  1. write `0xff00c00d` to `+0x7360`;
  2. wait until `+0x7364` bit 0 is set;
  3. clear it;
  4. configure `+0x7c20`, `+0x7c40`, `+0x7084`.

  This is declared as a command law on `0xff517000`, `0xff527000` and `0xff537000`.

## Other findings

- The long computation seen around 300 M instructions is an error‑correcting code over 32 KiB blocks of
  `0xFF` (the settings area in erased flash). It is not a hardware wait.
- Control requests (`0xff709000`): the firmware reads `+0x00` of channel 12 (`0x108d79c`, base table
  `0x13ea278`). *Upstream #184 models the whole 0x100 block: we will take theirs.*
- Block families seen:
  - 3 identical instances (`0xff61x/0xff517`, `0xff62x/0xff527`, `0xff63x/0xff537`), with "done" bits at
    `+0x670/674/678`, `+0xc18`, `+0x110` and `+0x090`;
  - 4 instances at `0xff5x5000` (busy bit `+0x384`);
  - a byte controller at `0xfff3e000` (likely I²C);
  - clock/reset blocks at `0xff70xxxx` and `0xfff48000`.
