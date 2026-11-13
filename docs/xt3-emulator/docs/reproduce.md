# Reproducing the X‑T3 emulator

You need a FujiMod checkout at a path **without spaces** (`$FUJIMOD`) and your own copy of the X‑T3 5.11
update file, `FWUP0019.DAT` (not distributed).

## Build

```bash
cd "$FUJIMOD"
git submodule update --init --depth 1
export LIBRARY_PATH=/Library/Developer/CommandLineTools/usr/lib/swift/macosx   # macOS without full Xcode
# fast build (opt-level 3, ThinLTO, native CPU), about 2.8x the speed of the original release build:
RUSTFLAGS="-C target-cpu=native" cargo build --locked --profile dist -p fujimod-emulator-xp4-board --example xp4_run
```

Binary: `target/dist/examples/xp4_run`. Rebuild it after **every** model change, otherwise it is stale.

## Run

```bash
python3 tools/nor_image.py FWUP0019.DAT run/nor.bin 0x4000000      # DAT -> 64 MiB NOR image, checks SHA-256
python3 tools/xt3_plan.py --nor run/nor.bin --out run/plan.json --cores 4 --quantum 20000 --budget 2500000000
target/dist/examples/xp4_run run/plan.json run/report.json
```

- `--discovery` builds a discovery plan. Strict plans also take the declared register blocks.
- Keep `quantum = 20000`. It sets how the 4 cores interleave, so changing it changes every instruction count
  quoted in these notes.
- Speed: about 5.4–5.9 M instructions/s on one host core. One run uses one core and about 300 MB, so run as many
  in parallel as you have performance cores.

## Tools

| Tool | Purpose |
|---|---|
| `nor_image.py` | DAT → NOR image |
| `xt3_plan.py` | catalog → board plan: RAM 1 GiB, power registers, milestone labels, loader preload |
| `touches.py` | adds key presses and captures, e.g. `1000M:UP 1290M:OK 950M:capture:name` |
| `instantane.py` | live snapshots: `serveur PLAN --a 950M` once, then `essai NAME BUDGET STEPS…` from the frozen state |
| `carte_sd.py` | adds a virtual SD card to a plan (`--standard --hote 0xfffc8000 --irq N`, `--absente`) |
| `appels.py` | adds a `watch_calls` action that traces calls (arguments, return values) over code ranges |
| `murs.py` | strict run‑to‑wall loop |
| `preuves.py` | discovery report → register‑block laws |
| `frontiere.py`, `dis.py`, `appelants.py` | analysis: frontier by page, disassembly, caller chains |
| `frames_png.py`, `tampon_png.py` | LCD frames and RAM buffers → PNG |

> Tool names and messages are in French. Paths in the tools still point to the author's machine and must be
> made relative before publishing (see [CHECKLIST.md](../CHECKLIST.md)).

## First‑start key sequence (from a snapshot at 950 M)

11 × UP every 25 M instructions from 1000M (FRANCAIS; 12 = ENGLISH), then 1290M OK, 1360M DOWN, 1390M DOWN,
1430M OK (time zone), 1500M OK (date), 1570M OK (app page). The shooting screen appears around 1.64 G. Then
2100M MENU closes the warning and 2250M MENU opens the menu, which is visible around 2.45 G.
