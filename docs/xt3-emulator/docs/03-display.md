# M3 — Display: first‑start pages and shooting screen

## First image

- With 1 GiB of RAM, `0x10e4920` (LCDC totals/porches) and `0x10f6010` (phase routes) run at 409.9 M
  instructions (strict).
- LCDC1 (`0xfffa9000`): picture layer enabled (`+0x08 = 0x11000`; bit 12 = picture, bit 4 = video), source
  `0x474d000`, **720 × 480 BGRA** (rear LCD).
- First captured image: the first‑start screen "言語/LANG.", page 2/5.
- **The strict run (no guessed value) and the discovery run produce the same image**: same SHA‑256,
  `13af1d4a…`.

## LCDC registers (`emulator-xp4-display/src/lcdc.rs`)

`+0x28`, `+0x30` (written 0, then `0x20080001`, read back) and the range `+0x40…+0x320` (including `+0x1c4`)
are accepted as plain latches. Tests: display 8/8, framebuffer 1/1, board 11/11.

## Shooting screen

First‑start sequence, driven by injected keys (see [04-keys-and-menus.md](04-keys-and-menus.md)):

1. language: a 5‑page list, with the help bar translated live;
2. OK → time zone (world map, GMT ±0:00);
3. down, down, OK → date/time (2018.1.1 0:00);
4. OK → smartphone app page (QR code);
5. OK → **shooting screen**.

The shooting screen shows the full OSD: card slots 1/2, image size L F, date, AF‑S, P, shutter, aperture F1.0
(no lens), ISO, DR100, exposure scale, touch AF. The viewfinder is a uniform grey: the model's declared scene
(luma 128).

All screens from language selection to the shooting screen are **bit‑identical in strict and discovery
mode**.

## Going to live view: opt‑in imaging extensions

When the camera enters live view, the firmware programs imaging registers that FujiMod's models refuse on
purpose ("do not pretend"). Their tests encode that refusal, so every extension is **opt‑in**. Only the X‑T3
XP4 wiring enables it, and the defaults are unchanged:

- `emulator-sensor-input/src/sensor.rs`: `ImageRoutes.latch_unrouted`. Image‑readout DMA registers and
  `+0x38` are latched without a DMA route: no image, no IRQ.
- `emulator-sensor-input/src/input_buffer.rs`: `Wiring.ignore_undeclared`. Undeclared channel‑enable bits
  are ignored.
- `emulator-xp4-display/src/movdc.rs`: `Movdc::latch_configuration()`. ISP coefficients, crop and modes are
  latched. The selected output mode is **not** emulated.
- `emulator-sensor-input/src/statistics.rs`: non‑decoded fields read back their SRAM bytes.

Tests: sensor 11/11, display 8/8, xp4‑io 27/27, XPP 119/119, XP5 imaging 9/9, XP4 board 11/11, XP5 board
builds.
