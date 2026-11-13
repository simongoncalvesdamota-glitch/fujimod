# PR draft — ip/sdhci: clock‑block word +0x2E

**Title:** emulator-ip: model the SDHCI +0x2E word (timeout control and software resets) in `sdhci::Clocks`

## Summary
X‑T3 5.11 resets the CMD and DAT lines of its SD hosts with 16‑bit writes to `+0x2E` (`0x200`, `0x400`), then
waits for the bits to clear. `sdhci::Clocks` only covered `+0x2C`.

- `Clock.timeout: u8`.
- Reset bits `0x0700` (all / CMD / DAT) clear themselves.
- The XP4 `sd_clocks` window now covers `+0x2c…+0x2f`.

## Tests
SDHC init 9/9, XP4 board 11/11. (`fingerprint_distinguishes_media…` failed before this change and is fixed
upstream.)
