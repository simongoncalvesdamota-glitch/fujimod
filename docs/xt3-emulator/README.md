# Fujifilm X‑T3 (X‑Processor 4) full‑system emulation in FujiMod

Bring‑up notes for emulating the **Fujifilm X‑T3, firmware 5.11** on FujiMod's XP4 board
(`crates/emulator-xp4-board`). The goal is to reach the same "Emulator" track as the X70 on
fujimod.com: boot, record a picture/movie, controls, menus, focus, lens, film simulations,
playback, RAW development, SD card, and the stock updater.

> **Status: first publication (2026-11-13), emulator-only.** Everything that can be done without a real X‑T3
> is done. A second publication will follow once the remaining facts are measured on a real camera (see
> "Still to measure on hardware" below).

## Progress

| Milestone | Status | Notes |
|---|---|---|
| M0 Build `fujimod-emulator` | ✅ | [docs/01-build-and-boot.md](docs/01-build-and-boot.md) |
| M1 XP4 boot path (loader → kernel → ThreadX → application) | ✅ | decompressed units match `fwtool.py` (SHA‑256) |
| M2 Peripherals | ✅ strict | [docs/02-peripherals.md](docs/02-peripherals.md) |
| M3 Display: first‑start pages, shooting screen | ✅ strict | bit‑identical to discovery mode — [docs/03-display.md](docs/03-display.md) |
| M4 Keys and menus | ✅ strict | every tab and sub‑menu; a setting applied end to end — [docs/04-keys-and-menus.md](docs/04-keys-and-menus.md) |
| M5 Play back a picture | ✅ | SD card mount, JPEG decoder, resizer, Exif — [docs/05-sd-card.md](docs/05-sd-card.md) |
| M5 Take a picture (S2) | ✅ | RAW frame → JPEG written to the card, test chart comes out exact — [docs/07-photo.md](docs/07-photo.md) |
| Movie (record + play back `.MOV`) | ✅ | 4K mode, live audio DMA, encoder, RESZ 4K, `.MOV` written and played back — [CHANGELOG.md](CHANGELOG.md) |
| M6 SD card boot + stock updater | ⏳ 5/6 | dual boot and the update screen work; `ffinit.bin` runs only under an assumed boot ROM |
| M7 Regenerate progress (`progress.generated.json`) | ✅ | XP4 SoC profile in upstream format, full replay the FujiMod way |

## Still to measure on hardware

These points cannot be settled in the emulator alone. Each one is documented with its evidence and a
reliability rating.

- **Boot ROM**: it is mask ROM inside the SoC and is not in the firmware. Read it on a real camera (closes M6).
- **Audio converter at `0xFFFA8000`**: the exact sample law. The model declares one, rated 50 %.
- **Y0L2 frames**: the spatial order of the four Y values (bit positions are sourced, 90 %).
- **Power‑save view**: what the hardware returns in sensor mode `0x8d`, so that a half‑press wakes the live
  view — [docs/08-power-save-view.md](docs/08-power-save-view.md).

"Strict" means every MMIO access is modeled or declared with evidence. No value is guessed at run
time. "Discovery" is FujiMod's tolerant mode: it lets unknown accesses through and probes polled bits.

## Contents

| Path | What it is |
|---|---|
| [CHANGELOG.md](CHANGELOG.md) | dated log of every step, newest first |
| [docs/](docs/) | technical write‑up per milestone, with the evidence behind each fact |
| [docs/reproduce.md](docs/reproduce.md) | how to build and run the X‑T3 emulator from a FujiMod checkout |
| [docs/fujimod-changes.md](docs/fujimod-changes.md) | every change made to FujiMod crates, with test results |
| [drafts/](drafts/) | ready‑to‑post GitHub texts: one bring‑up issue, then one file per pull request |
| [CHECKLIST.md](CHECKLIST.md) | what to check before anything goes public |

## Rules this work follows

- **No Fujifilm firmware bytes** are published: only addresses, sizes, register values and SHA‑256 digests.
  Users supply their own `FWUP0019.DAT`.
- Nothing is flashed to a real camera without a full backup of its internal flash and without first
  validating dual boot from the SD card.
- Facts that can only be measured on the real camera are marked **"to measure on hardware"**.
