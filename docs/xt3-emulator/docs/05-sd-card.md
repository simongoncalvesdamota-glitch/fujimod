# M5/M6 prerequisite — SD card (in progress)

Every stock feature that saves or reads a file needs a working card, and so do the M6 steps (boot from the
card, stock updater). This page records what is established so far.

## Hardware layout seen by the firmware

- **Two standard SDHCI slots** at `0xfffc4000` (n = 0) and `0xfffc8000` (n = 1). The driver
  (`0x1085xxx–0x108axxx`) computes `base = 0xfffc4000 + (n << 14)`. The X‑T3 has two card slots.
- A **UHS‑II bank** per slot at `0xfffc6200 + n·0x4000`: accesses at `+0x3e` and `+0xc2`.
  Its page also has vendor words at `+0x78`, `+0x80` and `+0xec`.
- Vendor words after each SDHCI bank: `+0x1e0` and `+0x1e4` (read‑modify‑write, e.g.
  `+0x1e0 = 0x10000 → 0x10400 → 0x400`, `+0x1e4 = 0xff0101`).
- **Card detect = GPIO port 10, pin 4, active low.** Pin at 1: "NO CARD". Pin at 0 from boot: "CARD ERROR".
- SD interrupts: GIC **230** (slot 0) and **237** (slot 1), plus 235 and 242 for the UHS‑II banks. They come
  from the firmware's interrupt‑source table (see "Call traces" below).
- Slot pin setup: `0x108552c(n)` configures GPIO pins `0x80008`/`0xe0001` (slot 1) and `0xe0006`/`0xe0000`
  (slot 0) through `0x1071550`, `0x107159c`, `0x10714c0`, `0x10715e0`.

## FujiMod additions (opt‑in; defaults unchanged)

- XP4 board: a new `sd_hosts` declaration. It attaches FujiMod's generic SDHCI host and a virtual FAT32 card
  (with an optional IRQ) at any base, with a window of `+0x000–0x107`.
- `emulator-ip/src/sdhci/host.rs`:
  - `Options.capabilities` (`+0x40/+0x44`, read‑only). The X‑T3 value is unknown, so we declare an
    assumption, `[0x0160C8B2, 0]`: 3.3 V, SDMA, high speed, 200 MHz base clock, no ADMA2, no UHS. **To measure
    on hardware.**
  - `Options.line_levels`: present‑state CMD/DAT/CD/WP levels, plus halfword reads of `+0x24/+0x26`.
    The driver requires DAT[3:0] high (`0x10857ec`).
  - Host Control 2 bit 12 (Host Version 4 Enable) is latched without effect.
  - Host Control 2 bits 4–5 (Driver Strength Select) are latched without effect. The X‑T3 writes `0x30` after
    CMD6 (`0x1086ec8`). 1.8 V signaling (bit 3) is still refused.
- `emulator-ip/src/sdhci.rs`: the `+0x2E` word of the clock block (timeout byte and self‑clearing
  CMD/DAT/all reset bits). The firmware resets the CMD/DAT lines with 16‑bit writes `0x200`/`0x400` and waits
  for them to clear.
- `emulator-xpp/src/sd.rs` (X‑Processor Pro SD wrapper):
  - `Options.extra_vendor`: extra vendor words, any access width;
  - `Options.capabilities`, passed through to the host.
- `xp4_run` action `gpio`: drives an input pin between slices, for example a card insertion.

Tests: SDHC init 9/9, XPP 119/119, XP4 board 11/11, emulator‑ip 46/47. The one failure,
`fingerprint_distinguishes_media…`, already failed before our changes; upstream has since fixed it.

## Experiments

| Setup | Result |
|---|---|
| Card on the UHS‑II pages (`0xfffc6000`, `0xfffca000`) | configuration only, no card command |
| Card on `0xfffc4000` or `0xfffc8000` from boot, any of IRQ 66/67/167/168 | no card command; "CARD ERROR" |
| No card at boot, then insertion (GPIO 10.4: 1 → 0 at 1.75 G), card on `0xfffc8000` | the firmware reacts (display layers off), but sends no card command |

Detail of the insertion run, which was the same in strict and discovery mode:

- On `0xfffc8000`, the host is configured:
  - Host Control 1 / Power `+0x28 = 0x0fc2`: 4‑bit bus, card detect forced by the test level, bus power on
    at 3.3 V;
  - Clock `+0x2c = 3`: internal clock on and stable, **SD clock still off**;
  - timeout `+0x2e = 0xe`;
  - no interrupt enables.
- Discovery mode finds **no polled register** after the insertion. The driver is not stuck in a poll loop.
- Calls traced after the insertion:
  - `0x108503c(1, 2, …)` at 1.76 G on core 3;
  - at 1.98 G, function `0x1081114(slot)`. It calls `0x1083984`, then `0x1085f08(0/1, slot)`,
    `0x1084fe0(slot, 0)` → `0x1086a58(slot, 0, 0)`, `0x1084f74(slot)` → `0x10868f8`/`0x1086830`/`0x1086898`,
    and `0x108552c(slot)` (pins back to rest). This looks like a **slot power‑off**.
- `0x1081114` has no direct caller in unit 5. It is called from the **card manager** in units 3/4: at
  `0xd25568`, from a "close" routine (`0xd25500(slot)`). That routine clears the slot's user entry,
  decrements a user count, and powers the slot off when the count reaches 0. The matching "open" routine is
  `0xd25570(slot)`.

## Call traces (`watch_calls`, ~2 370 entry points, 4 parallel strict runs)

- **Units 3 and 4 are two instances of the same card manager, one per slot.** Unit 3 (`0xd2xxxx…0xd4xxxx`)
  serves slot 0, and unit 4 (`0xe2xxxx…0xe4xxxx`) serves slot 1, with driver handle `0x1004f`.
- What happens on insertion:
  1. An event dispatcher (`0x1131a38`) calls `0xe36220(0x28, 2)` → `0xe363d4` → `0xe25b34` → `0xe24fb4` →
     `0x108136c(0x1004f, 2, 1)` → `0x108503c(1, 2, …)`.
  2. `0x108503c(slot)` takes the event group stored at slot table `0x155dc08 + 0x30·slot + 0x10` and sets flag
     `0x8000` on it (`0x1134714`). This **aborts the wait of a pending command**.
  3. About 220 M instructions later, the media‑change handler `0xe28f54` unmounts the slot, which powers it
     off (`0xe36354` → … → `0xe42928` → `0xe25500` → `0x1081114`). It then tries to remount
     (`0xe36068` → … → `0xe25a24` → `0xe24e14`), which fails (`0xe36088` → −1).
  4. `0xe48f8c` always returns `0xffffff01`. It is a stub, not the cause.
- **What the driver waits for.** `0x1084170(slot, …)` sends one command:
  1. `0x1083c10` registers the current thread's event group in the slot table.
  2. `0x1083c3c` installs the interrupt handler for source `0x59…0x5c`.
  3. `0x1086144` enables the interrupts.
  4. The argument goes to `+0x08/+0x0a`.
  5. `0x1086940` waits for CMD/DAT inhibit (`+0x24` bits 0–1) to clear, then writes `+0x0c` (32 bits).
  6. `0x1084028(slot, 0x64)` waits up to 100 ms for the event.

  The interrupt handler (`0x1083cd8`) reads `+0x30` and sets `(status << 16) | 0x8000` on the event group.
  With a card at boot, **CMD0 is written** (`0x1086940(1, 0)` returns 0), but the wait times out (returns 4):
  no interrupt ever arrives.
- **The SD interrupt lines come from the firmware's interrupt‑source table** at `0x13e6454` (16‑byte
  entries: GIC ID, priority, type, target). The numbering is the same as the catalog's: the serial flash is 37
  and the timers are 57–64, both present in the table.

  | Source | GIC ID | Handler | Bank |
  |---|---|---|---|
  | `0x59` | **230** | `0x1084004` | slot 0 SDHCI (`0xfffc4000`) |
  | `0x5a` | 235 | `0x1083fe0` | slot 0 UHS‑II (`0xfffc6200`) |
  | `0x5b` | **237** | `0x1083fbc` | slot 1 SDHCI (`0xfffc8000`) |
  | `0x5c` | 242 | `0x1083f98` | slot 1 UHS‑II (`0xfffca200`) |

  The handler computes the bank from `(slot + 0x3fff1) << 14` plus `mode << 13`, plus `0x200` in UHS‑II mode.
  This matches the bases above. The lines tried before (66, 67, 167, 168) were wrong.

## Card recognized

With IRQ 230/237, the driver completes the SD initialization:

- the last command seen is CMD6 with data (`+0x0c = 0x063a0011`), with R1 = `0x900`;
- then Host Control 2 is set to `0x30`;
- the card is then mounted. It reads the MBR (sector 0), the partition boot sector (sector 8192), the FAT
  region and the root directory: 1 050 112 bytes in all.

This holds in strict mode, with no wall up to 2.4 G instructions.

| Setup | Screen |
|---|---|
| card on slot 0 (`0xfffc4000`, GIC 230), GPIO 10 = 0 | **545** remaining shots, "1 CARD PROTECTED" |
| card on slot 1 (`0xfffc8000`, GIC 237), GPIO 10 = 0 | "1 CARD ERROR" (slot 0 is empty but its card detect says present) |

- Driver slot 0 = camera slot "1".
- Card detect does not gate the SD initialization: the driver forces card detect through the host's test
  level (`+0x28` bits 6–7). GPIO 10.4 is what the UI uses.
- Slot pins: `0x1085d94` muxes `0xa0003`/`0xa0004` (port 10 pins 3/4) to function 1 for slot 0, and
  `0xa0007`/`0xa0008` to function 2 for slot 1 (`0x10714c0`).

## Card detect and write protect

| Signal | Where the firmware reads it | Level |
|---|---|---|
| Card detect, slot "1" (driver 0) | GPIO 10.4, scan‑table key 13 "CARD" | active low |
| Card detect, slot "2" (driver 1) | GPIO 10.8, scan‑table key 14 | active low |
| Write protect | SDHCI Present State bit 19, read by `0x10878ac(slot)` | **active high** (1 = protected) |

- Write protect: GPIO 10.3 at 0 or 1 makes no difference, and bit 19 set gives "CARD PROTECTED". The new host
  option `write_protect_active_high` reports bit 19 = 1 only while the card is protected.
- Public corroboration (to re‑check against the kernel source before publishing): the controller belongs to
  the Fujitsu/Socionext **F_SDH30** family. The Linux drivers `drivers/mmc/host/sdhci_f_sdh30.c` and
  `sdhci-milbeaut.c` set `SDHCI_QUIRK_INVERTED_WRITE_PROTECT`, which is the same inversion. The same header
  names the vendor words after the SDHCI bank:
  - `+0x100` AHB_CONFIG;
  - `+0x108` TUNING_SETTING;
  - `+0x114` IO_CONTROL2 (1.8 V switch);
  - `+0x124` ESD_CONTROL;
  - `+0x158` TEST (bit 6 FORCE_CARD_INSERT).

  The X‑T3 driver writes `+0x100 = 7` and touches `+0x114` and `+0x124`.
- FujiMod's XP5 "SD interface" model (`emulator-xp5-io/src/sd_interface.rs`, a block ending at `+0x1e0`) may
  be the same block as the X‑T3 words `+0x1e0/+0x1e4`. To try instead of the plain register file.
- With both slots wired (`0xfffc4000`/GIC 230 and `0xfffc8000`/GIC 237) and both detects low, both cards are
  mounted.
- For the catalog, the X‑T3 is the first body with **two card slots**. The `card_slot` schema accepts only
  one, so it needs extending. FujiMod's X70 already wires detect and protect through one shared `Contacts`
  object, which is the model to follow.
