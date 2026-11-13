# PR draft — catalog: X‑T3 5.11 board facts (XP4 register maps)

**Title:** catalog: X‑T3 5.11 — XP4 register maps that boot to the shooting screen in strict mode

## Summary

Our X‑T3 board definition runs the stock 5.11 firmware in **strict** mode (no unmapped access). It starts from
the loader with no boot ROM, then:
- completes first start and the menus;
- shows the shooting screen and plays back a JPEG;
- starts an ES capture (capture still waits on the image pipeline).

This PR moves what the current `HardwareRegisterLayouts` schema can already express into
`catalog/soc/xp4/soc.json` → `hardware.profiles.xp4.register_maps`. It is based on #184.

- **`register_blocks`: 71 entries.**
  - 70 blocks from the strict runs: 9 with `idle_bits` and 12 with `commands`. Each one was recorded in
    discovery mode and then replayed in strict mode.
  - `0xff70b000` from #184.
  - Not kept: #184's `0xfff3e000`. That page is a Socionext F_I2C controller (battery gauge on the bus), not a
    last‑write bank. See "Not expressible yet".
- **`jpeg_decoder`** `0xfffa1000`, `irq_route` 136, and **`jpeg_encoder`** `0xfffa0000`, `irq_route` 135. Playback
  of a test JPEG runs 4 decoder jobs.
- **`dram_alias`** `0x40000000`. The capture DMA targets `0x4xxxxxxx`, the same uncached alias as the X70.
- **`serial_ports`** `0xfff39000`, `0xfff3a000`, `0xfff3b000`. These are three ARM PL011 r1p5 ports (PeriphID
  `0x11 0x10 0x34 0x00`); the updater talks to port 0. Needs a PL011 model behind `serial_ports`, unless one exists.
- Value note: the SD clock word `0xfffc402c` lies inside the SDHCI window of SD host 0 (see below).

The candidate `soc.json` deserializes with `fujimod_catalog::reference::Soc` and passes `Soc::validate("xp4")`.

## Body facts (`bodies/xt3`)

- RAM 1 GiB. Already in #184; the kernel MMU table maps `0x00000000–0x3fffffff`.
- Two card slots:
  - SDHCI at `0xfffc4000` (n = 0) and `0xfffc8000` (n = 1); base = `0xfffc4000 + (n << 14)`;
  - a UHS‑II bank at `+0x2200`;
  - card detect on GPIO port 10 pin 4, active low;
  - write protect active high (Linux `sdhci_f_sdh30` / `sdhci-milbeaut` invert it too).
- Power‑controller inputs:
  - 16 = 4 (power‑on reason);
  - 4 = battery voltage, threshold 752;
  - **48–55 = settings header** and **32/34/35 = RTC**. These belong to a "configured body" run preset;
    see draft 07.

## Not expressible yet (new models to discuss first)

1. Standard SDHCI host (`capabilities`, line levels, write‑protect polarity). `xpp_sd` assumes the XPP wrapper
   (`register_offset`, `reset_channel`).
2. Socionext F_I2C controller at `0xfff3e000` (GIC 66) and `0xfff3f000` (GIC 67). `xpp_i2c` is DesignWare.
3. 16‑bit timers at `0xfff3c000` (GIC 41).
4. Job blocks that `register_blocks.job` cannot express:
   - `0xfa000000` (GIC 145): the acknowledge is in another block, `0xfff4aa10` mask `0x8`;
   - `0xfff7e000` (GIC 132): resizer, status word `+0x10c`;
   - `0xfff7f000` (GIC 133): status word `+0xe4`.
5. Imaging options:
   - statistics SRAM window (128 KiB, so the second bank copy at `0xfff17000` fits);
   - sensor DMA lines 97/98;
   - frame start pulse on GIC 101.

## Tests
<catalog schema check + board tests after the rebase on main>

<sub>No firmware bytes: addresses, interrupt lines and values only.</sub>
