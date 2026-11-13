# M0–M1 — Build and XP4 boot path

## M0 — Building `fujimod-emulator` on macOS (Apple Silicon)

Extra prerequisites:

- `cmake` and `pkgconf` (Homebrew), needed by the Unicorn build.
- The pinned Unicorn submodule: `git submodule update --init --depth 1`.
- **A checkout path without spaces.** QEMU's `configure` inside Unicorn rejects it
  ("main directory cannot contain spaces nor colons").
- With only the Command Line Tools installed (no full Xcode), linking fails on
  `libswiftCompatibility56.a` (crate `apple-cf`). Fix:
  `LIBRARY_PATH=/Library/Developer/CommandLineTools/usr/lib/swift/macosx`.

Check: `fujimod-emulator boot --configuration xt3-511 …` answers
`native X70 boot does not support loader-start configurations`. The native boot path is X70‑only, so the
X‑T3 needs the XP4 board.

## M1 — XP4 boot path

FujiMod already has a complete but unused XP4 board (`crates/emulator-xp4-board`, driven by a JSON
`Definition`) and an SoC profile (`catalog/soc/xp4`) describing its wiring. What was missing: a bridge from
the catalog to the board, a loader start, and one fix.

1. **`Session::run` fix**: `until: Some(u64::MAX)` → `until: None`. Before the fix, the backend rejected the
   empty range and failed with `UC_ERR_ARG` on the first instruction. Board tests: 11/11 (6 failed before).
   *Already proposed upstream in #183, #184 and #185: not part of our pull requests.*
2. **Run bench** `examples/xp4_run.rs`: JSON plan → `Session`, RAM preloads, `run_board`, JSON report
   (registers, frontier, interventions, milestones, dumps, LCD frames, device state, GIC state). Scheduled
   actions run between CPU slices with all cores stopped: `key`, `frames`, `gpio`, `watch_calls`. Live
   snapshots by process fork (`branch_at`, `branches`, `serve`).
3. **Plan generator** (`xt3_plan.py`): catalog (`xp4.register_maps`, `xt3-discovery` entries, firmware sheet) →
   the 28 `Definition` fields. All XP4 board validations pass.
4. **Loader start**:
   - NOR image = `FWUP0019.DAT` XOR 0xFF from offset 0x274, padded to 64 MiB with 0xFF.
     The padding is an assumption: erased flash.
   - The NOR loader at 0x20000 (256 KiB, vectors linked at 0) is copied to RAM 0.
   - Hand‑off block at 0x900000 (+0x140 = 4); PC = 0.
5. **Four cores are required.** The loader is multi‑core: a secondary core runs the flash‑read worker
   (0x24bc), while core 0 waits for `*0xb534 == 2`. With one core, it deadlocks.

Milestones (4 × Cortex‑A7):

| Milestone | PC | Instructions |
|---|---|---|
| Loader reset (4 cores) | `0x100` | 2 → 134 |
| Kernel entry | `0x578a8` | 14 478 812 |
| ThreadX kernel entry / thread create | `0x41f6c` / `0x441ac` | 15.33 M |
| ThreadX scheduler (4 cores) | `0x400c8` | 24 843 092 |
| Initial application thread | `0x1147a4c` | 24 847 934 |

**Independent check:** after boot, the emulated RAM holds units 0 (kernel), 3, 4 and 5 (application,
compressed with method 4). They have the **same SHA‑256** as the units extracted by `fwtool.py`. Units 1, 2
and 6 are loaded later, on demand.
