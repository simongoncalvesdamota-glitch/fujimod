# PR draft — XP4 board: exact fast paths and opt-in counted execution

**Title:** emulator-xp4-board: run the ARMv7-A cluster in counted native blocks (opt-in), and cut per-instruction overhead

## Summary
A `sample` profile of an X-T3 run shows 96 % of the time inside Unicorn's per-instruction code hook: the XP4 board
installs `Hook::Instruction` on `HookRange::All` and does its bookkeeping before every instruction, while the
generic board already runs Cortex-A7 cores on the counted `aprofile::Core` backend. This PR keeps the XP4 execution
law and changes only how fast it is evaluated.

### 1. Exact fast paths of the per-instruction mode (default, always on)
- Unicorn (vendored fork): `JIT_CALLBACK_GUARD` restores the JIT write protection only when the callback changed
  it (a per-thread toggle counter in `jit_write_protect`), instead of an ISB-serialised system-register write
  after every hook call; the debug-only `assert_executable` read is dropped from the hot path.
- `Cluster::word`: an instruction fetch that translates into plain DRAM is read from the RAM backing; the
  observational `read_virtual` (page-size query, region-list copy, physical read, allocation) remains for the low
  boot window, aliases and ROM/SRAM windows.
- A 64 Ki-bit filter of label, pending-return and PC-observer addresses skips the exact lookups for every other
  instruction (bits are never cleared, so a set bit only means "look it up").
- `devices_due` checks the cached deadline without moving the device set out of the run state; the interval
  timers are walked only once a counter is due (`expire` is a no-op before).

### 2. Counted execution, opt-in with `FUJIMOD_XP4_COUNTED=1`
Each core is wrapped in `aprofile::Core` with two new owner options: `set_owner_exclusives` (LDREX/STREX/CLREX exit
before entry like the framework instructions) and `set_owner_interrupts` (an asserted, unmasked IRQ stops the run
before the next block instead of being entered by the adapter). The board keeps every step of the per-instruction
hook at the same instruction boundary:
- a scheduler slice is cut into internal runs that end exactly where `due()` has work (`Ratio::budget` to the next
  device deadline or interval-timer expiry; the per-core-time phase is applied in bulk with the same formula);
- MMIO accesses resynchronise guest time from `instruction_progress` and `tighten` the run to stop after their
  instruction when `due()`, the cache redirect or an IRQ would act before the next one;
- the board enters IRQs, performs exclusives / identity reads / WFI/WFE/SEV through the existing code, and ends the
  slice exactly where the hook ended it (Unicorn's count hook stops before the board hook at the slice end, so no
  boundary work happens there);
- labels are single-address observers; call returns get observers installed at the next internal boundary;
- the exclusive-monitor store hook is a *watched* hook: only stores to the 1 KiB pages of armed monitors reach it
  (an unwatched hook would end a block at every store under a translation filter);
- device RAM writes report the written bytes to each engine (`backing_written`), which invalidates only translated
  code they touched, instead of discarding every translation; the slice still ends as before.

Not kept in counted mode: the per-core trace of the last 32 entered instructions (`state.history.recent`). Refused
with an explicit error: driver guard, memory and function watches, instruction points, discovery, the low boot
alias and the PMU profile (they need every instruction; run without the variable).

### 3. Coverage and indirect edges (counted mode)
`Session::enable_indirect_edges` / `indirect_edges`: a block observer records, per core, where a computed branch
(`blx rN`, `bx rN` except `bx lr`, `ldr pc` / `ldm … pc` not from the stack, `mov`/`add pc`) actually went; IRQ and
abort entries and board-performed instructions reset the predecessor. Bench action `indirect`.

`Session::enable_coverage` / `take_coverage` drain the addresses the counted engines translated (each one entered
code). The bench action `{"kind": "coverage", "label": …}` writes the instructions entered for the first time since
the previous one, e.g. around a half-press and a full press of the shutter.

## Evidence
Reports compared key by key (everything except wall time), all scan-out frames and RAM dumps byte for byte, against
the unmodified binary built from the same tree (`dist`, `target-cpu=native`, quantum 20 000):

| Plan | Content | Per-instruction (fast paths) | Counted |
|---|---|---|---|
| R2, 600 M | boot to the shooting screen from a NOR left by a clean power-off | identical | identical except `recent` |
| R2, 1.2 G | same, 8 frames | — | identical except `recent` (370 s → 23 s, both under load) |
| PCH, 2.89 G | keys, scene, `time_base` [40, 1], half and full press, 2 dumps | identical (775 s → 181 s) | identical except `recent` (775 s → 56 s) |
| K32, 2.9 G | SD card, playback of a JPEG (decoder on GIC 136), 13 frames | identical (844 s → 237 s) | identical except `recent` (844 s → 161 s, GPU-bound playback) |
| N6, 3.3 G | configured body, dial, 15 frames | identical (967 s → 313 s) | identical except `recent` (967 s → 140 s) |

Durations above were measured while up to 7 runs shared 10 cores, so ratios are indicative. Fork snapshots: a branch
forked at 600 M and run to 1.2 G is identical in counted and per-instruction mode.

## Tests
XP4 board 12/12 (default mode); in counted mode 7/7 outside the `guarded_*` tests, which get the explicit refusal; `emulator-cpu-unicorn` 206/206.
