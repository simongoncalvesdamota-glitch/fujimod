# Issue draft — X‑T3 5.11: XP4 emulator bring‑up

**Title:** X‑T3 5.11: XP4 board boots to the shooting screen and menus (strict mode) — notes and findings

---

Hi! This is a report on bringing up the **X‑T3, firmware 5.11** on the XP4 board (`emulator-xp4-board`). It
complements #183/#184 (X‑T3 XP4 register blocks) and #132 (X‑T30 static facts, same SoC).

**Where it stands**

- Boot from the NOR loader on 4 cores → kernel → ThreadX → application. Decompressed units match
  `fwtool.py` (SHA‑256).
- In strict mode (every access modeled or declared with evidence), up to 3.2 G instructions: first‑start
  pages, shooting screen, menu and sub‑menus driven by injected keys. The images are bit‑identical to
  discovery mode.
- SD card: <fill in at publication time>.

**Firmware facts that may be new for the catalog**

- RAM is 1 GiB: the kernel MMU table identity‑maps `0–0x3fffffff`. The catalog says 512 MiB.
- Power controller (SPI1): register 16 = power‑on reason. The application powers off unless bit 2 is set:
  loader `0x15ec` → kernel → `0x1147750`. Register 4 = battery voltage, with firmware threshold 752
  (parameter `0x3eb`). Registers 13 and 27 are read too.
- Card detect = GPIO 10.4, active low. SD slots are SDHCI at `0xfffc4000`/`0xfffc8000`, with UHS‑II banks at
  `0xfffc6200 + n·0x4000`.
- Key queue at `0x15634fc` (count `0x156360c`), useful for driving the UI in emulation.
- Memory controller: 4 channels, table at `0x13eee58`, with a command/acknowledge handshake at
  `+0x7360/+0x7364`.

**To measure on a real body** (any help welcome): SDHCI Capabilities, power‑controller registers 13/27, RAM
size, boot ROM.

Details, evidence and reproduction steps: <link to the published notes>.
No firmware bytes are included anywhere: only addresses, values and digests.
