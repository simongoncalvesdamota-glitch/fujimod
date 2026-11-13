# PR draft — xp4-io: X‑T3 power‑controller registers

**Title:** emulator-xp4-io: accept the power‑controller registers the X‑T3 5.11 reads and writes

## Summary
The X‑T3 5.11 application reads power‑controller registers 13 and 27, and writes 13, 14, 16, 17 and 37.
`PowerSerial` rejected these accesses, which stopped strict runs (`Unsupported power SPI read register27`,
`write register14`).

- Inputs accepted (read‑only, value from the declaration): `0..=6, 13, 16, 24, 27, 32, 34, 35`.
- Writable (reads back the last write; until then, the declared input or 0): `2, 13, 14, 16, 17, 18, 32, 34,
  35, 37, 38, 48..=55`.

## Evidence
- Reads: every `mov r0,#n ; bl 0x10a1d40` call site gives 0, 2, 3, 4, 5, 6, 13, 16, 27, 35, 38.
- Writes: every call site of `0x10a20d4` gives 2, 13, 14, 16, 17, 18, 32, 34, 35, 37, 38.

## Tests
`cargo test -p fujimod-emulator-xp4-io`: 27/27. `-p fujimod-emulator-xp4-board`: 11/11.
