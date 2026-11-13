# PR draft — xp4-display: LCDC latches used by the X‑T3

**Title:** emulator-xp4-display: latch LCDC registers +0x28, +0x30 and +0x40…+0x320

## Summary
X‑T3 5.11 programs LCDC1 (`0xfffa9000`) registers that the model rejected:

- `+0x30`: written 0, then `0x20080001`, and read back;
- `+0x1c4`: written 0;
- `+0x28`.

They are now plain latches, with no effect on scanout. With them, the first‑start screen renders (720 × 480
BGRA) in strict mode, bit‑identical to discovery mode.

> **Watch note (2026‑11‑07):** upstream #184 already admits LCDC `+0x28…+0x3c` and `+0x40…+0x320`, which
> covers ours. If #184 merges, drop this PR, or reduce it to the strict‑mode render evidence below.

## Tests
display 8/8, framebuffer 1/1, XP4 board 11/11. *Needs a rebase on the upstream `lcdc.rs` refactor.*
