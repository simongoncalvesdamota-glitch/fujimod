# Changelog

Newest first. Instruction counts are emulated instructions on all 4 cores, with quantum 20 000.

## 2026-11-13

### Power-save view — the sensor mode is chosen by the firmware, not by a setting
- Details: [docs/08-power-save-view.md](docs/08-power-save-view.md).
- In power-save view, the sensor runs in mode 0x8d. In that mode the imaging pipeline never reports completion,
  so a half-press does not bring back the normal live view (100 %).
- Mode 0x8d comes from the firmware's own "enter power-save" request. The request sets a flag, the flag becomes
  mask bit 2 in a settings set copied for power-save, and the mask bit selects the power-save column of the mode
  table (100 %, measured in 7 runs).
- No persistent setting is read on this chain (85 %), so the settings bank is not the cause.
- Open, to measure on hardware: on a real body, the half-press is expected to leave power-save (70 %). If so,
  something the hardware delivers in mode 0x8d is still missing from the emulator.

## 2026-11-11

### Static analysis — X-T3 `analysis` facts packaged as a proposal for FujiMod
- FujiMod writes the official locators; we propose evidence. Draft issue (not posted):
  [issue-xt3-511-locators.md](issue-xt3-511-locators.md), in the format of upstream issues #132 / #167.
- A local deterministic checker re-finds every value by pattern (no result address hard-coded) and checks it
  independently; two runs give identical output.
- Results: `usb_normal_ids` 04CB:02DD (100 %); `ui_modes` play 4 / setup_play 6 (90 %); `menu_cursor`
  0x017E2A0C via set_node 0x01B628D0 (90 %); `framebuffer` 0x0474D000 720×480 (90 %, emulator only);
  `remote_menu`, `ff80_limits`, `service_exit.jig_deinit_link` partial (50 %); `app80` not found.
- The service dispatcher 0x02143150 is rejected as the FF80 debug dispatcher: it is the Wi-Fi PC-Shutter
  service (`wifi_pcsh`).

## 2026-11-10

### M7 — synthetic lens: focus-motion end works; the aperture table is empty
- A new runner action, `lens_motion_end`, raises the lens "motion done" line a fixed delay after each time the
  firmware pulls it low. It is off unless a plan arms it, and it must be armed after boot.
- Combined run (lens header mask + aperture limit 2 + this action), branched from one state, one run per branch
  (90 %):
  - The action closes each lens movement in both branches. The firmware then sends its normal follow-up
    request instead of repeating the waiting one.
  - Manual focus: the distance scale is shown, the shot is taken and the JPEG is written. Its image data is
    bit-identical to the same plan without a lens; only EXIF differs (lens name present, FNumber 0).
  - AF-S: the camera shows a focus failure on half-press and does not fire. Most likely cause: there is no
    focus model, so contrast never changes (60 %).
- Aperture: half-press shows "F 0.0", otherwise "F ---". The firmware converts an aperture step into an
  F-number (in hundredths) through two tables in the data area of the lens memory image. Our synthetic lens
  only fills the start of that area, so both tables read as zero (90 %).
- Test with a synthetic F2.8 table (synthetic lens-memory content, not a fact about any real lens; nothing
  here says what a real lens stores), one run per branch (90 %):
  - Half-press now shows "F 2.8" in both branches. The manual-focus JPEG has FNumber 2.8 and an aperture
    value of 3.0; its image data is unchanged. Focal length is still 0.
  - AF-S still shows a focus failure and does not fire. So neither a zero F-number nor a missing motion end
    caused it. The lens model echoes the focus position the body asks for, and the emulator supplies no
    sharpness signal (no contrast or phase data). That missing signal is the most likely cause (60 %).
  - Caveat: the synthetic table also overwrote a few neighbouring fields; no other visible effect.
- Focal length (static reading, 90 %): it comes from a per-zoom row of eleven values, interpolated along the
  focus position and scaled by one header field. The row sits in the same empty data area, further in, so the
  synthetic lens needs a larger memory image to fill it. A test with a synthetic 23 mm focal length is ready.
- That test failed (synthetic lens-memory content, not a fact; one run per branch). With the larger memory
  image (36 blocks instead of 6) and a non-zero focal scale, the firmware reads the whole image. It then sends
  two extra lens commands once, and the body's lens-control loop stops running partway through boot. Both
  branches lose the aperture display and the focus-mode switch, and no shot is taken (100 % on the observed
  state, not replayed). Whether the cause is the larger image or the focal-length content is still open
  (50/50). A test that keeps the larger image but uses the earlier content is running.
- Static reading of those two commands: one carries a 15-bit value and the other four flags plus a 12-bit
  value. The body's own family mask blocks both, so the firmware answers them itself and they never reach the
  lens model (90 %). Their completion only reports "error or not", not the reply data. The lens model has no
  generic reply: a request family it does not declare stops the run with an error rather than being ignored.

### M7 — autofocus statistics: what the firmware reads each frame
- A passive MMIO trace (new runner action, bit-neutral) records which code reads the statistics memory. Six
  of the requested statistics outputs are read every frame, in manual focus as well as AF-S. Only one of them
  (the 16×16 RGB sums) is produced by the emulator today; the others read as zero.
- One output is read as a grid of up to 7×7 zones, two 16-bit values per zone; in manual focus it is read
  as 1×1. This is the most likely autofocus contrast grid (60 %).
- AF-S reads exactly the same outputs as manual focus, with the same layout, and requests the same
  statistics configuration. The zone grid stays at 1×1 in AF-S too. No autofocus-specific reader of the
  statistics memory shows up, so a focus model would write a contrast value into that grid first.

### M5 — firmware colour profile: where each reservation stands
The three colour options stay off by default. Each one turns on only when every point below reaches 90 %.
- **2×2 chroma matrix**
  - Enable bit and coefficient layout (one word per output: own gain low, cross term high): 100 %, read in the
    firmware.
  - Q10 scale: 90 %. The firmware writes 1024 as identity, and the neighbouring matrices of the same unit are
    Q10. The hardware shift itself is inferred, not seen.
  - Cb/Cr order of the two outputs: 50 %. The unit's other matrices put Cb first. An older body's colour law
    puts Cr first. No emulator run can settle this, because the output follows the model we choose.
  - A test with real photos is ready. It uses a film bracketing (one shot, several film simulations) and
    compares the hue rotation between films, zone by zone, with what each order predicts. Stages common to
    all films cancel out.
    - Expected gap between the two orders: about 6–7° RMS on two film pairs, up to 13° on some hues.
    - Decision rule: the better order must fit within 3°, and the other must be at least 3° worse and three
      times worse, on at least two pairs.
    - On 400 synthetic trials it never gave a wrong answer.
    - Best reachable reliability: 70 %, since the planned photos come from a different, newer body.
- **Luma-dependent chroma bands**: law 90 %. Y is untouched and chroma follows the predicted scale on three
  films.
- **Brightness gain**
  - Table, input weights and Q9 output: 90 %.
  - Segment bounds: 90 %. They are one octave of brightness each (4, 8, 16 … 16384), read in the firmware's
    tables. The emulator code already used these bounds. A unit test now pins them down; this is a doc and
    test change only.
  - Whether brightness is measured above the black level: still 50 %. A call-trace run found the parameter
    block that is actually used (the first run had read a neighbouring, unused slot). The three per-channel
    fields of the block are reproduced exactly from the traced inputs: an offset of 32 on a 14-bit scale (the
    firmware derives it from a capture-context value of 2048), a scale of 64 and per-channel gains of 302,
    573 and 583. The offset is 32, not the emulator's raw black level of 1024, so this stage gives no
    evidence that brightness is taken above black.
  - Meaning of that offset: 90 %. It is a sensor black-level offset. The firmware computes it from a
    per-channel black measurement and clamps it to a floor of 2048 read from the factory settings bank
    (the value 2048 comes from that floor). The gain then scales the range above this offset. It does not
    depend on RAW/JPEG, quality or size. In the emulator the black measurement is zero, so the floor
    always applies, whatever the film simulation. On a real camera the offset may be larger.
  - Placement before the colour matrix: 50 %.
- **Profile**: off. Nothing changes in default runs (100 %, bit-identical).

### M7 — synthetic lens: the lens image header decides which requests reach the lens
- The body keeps a copy of the lens's own memory image. A bit mask in its header says which request families
  the body may send to the lens. When a family's bit is clear, the firmware does not send the request: it
  answers it locally, with zeros.
- An emulated lens whose header blocks the aperture and status families therefore reads as "aperture step 0",
  whatever the lens model would have answered, and the shooting screen stays incomplete (100 %, code and
  measured request trace).
- A synthetic header needs only a few fields to be consistent: the header check pattern, the family mask for
  the families the lens model answers, and one count field that bounds the aperture step the body accepts. All other fields can keep the firmware's
  built‑in default lens values. No real lens data is used.
- With such a header, the shooting screen settles in its normal state (frame counter, AF tab, focus mode) in
  one run, replayed bit‑identically (100 %). The aperture value is still shown as "F ---".
- The lens position request is bounded by another count field of the same header. The default value allows
  only position 0, so no answer to that request can change what the body shows (90 %, code reading). It is
  probably not the cause of "F ---".
- New runner option, off by default: end of lens movement. Before each lens movement, the firmware pulls a
  handshake GPIO line low and waits for an edge. The option raises the line again a fixed delay after each such
  write, which raises the interrupt the firmware waits for. With the option off, runs are bit‑identical to the
  earlier build (100 %). With it on: not yet measured.

### M5 — firmware colour profile: three models, kept off by default
- Three colour stages programmed per film simulation now have emulator models, each behind an option that is
  off by default: a 2×2 chroma matrix, Y‑dependent chroma bands, and a brightness gain applied before the
  colour matrix. Details and reliability per stage: [07-photo.md](docs/07-photo.md#firmware-colour-profile-three-options-all-off-by-default).
- With the options off, runs are bit‑identical to the earlier build (100 %). Each option changes only the JPEG,
  never the live‑view frames.
- Measured on three films: the luma bands leave Y untouched and scale chroma as the law predicts; the
  brightness gain lifts mid‑greys by 16 to 27 JPEG levels.
- They stay off until each stage reaches 90 %: the chroma axis and scale, and the brightness segment bounds and
  input scale, are still hypotheses (50 %). They will then be turned on together as one profile.
- New measuring tool: mean Y/Cb/Cr read straight from the JPEG DC coefficients, with no RGB round trip, which
  had clipped saturated colours.

### M7 — movie playback: picture delay confirmed as the cause; 16:9 placement explained
- Regression run on a build whose decode model now defaults to a 10 ms delay per picture. The run plan sets no
  picture delay and uses the default 48 000 B/s audio rate.
  - The run is bit-identical to the earlier successful playback run, which set the delay in its plan and used the
    camera's real audio rate (384 000 B/s). All 52 captured screens, the final state hash, the 9030 display
    frames, the instruction count and the tick count are identical.
  - All 60 frames are read, decoded and shown, and playback ends once. The bar reaches 100 % with "1s", then the
    camera returns to the clip screen.
  - So the audio rate plays no part: the picture delay alone unblocks the bar and the counter.
- Why the movie sits 37 lines down the 720×480 panel, read from the 5.11 playback code:
  - the player places the video layer at y = (480 − 720 × height / width) / 2;
  - for a 1920×1080 clip that is (480 − 405) / 2 = 37, which matches the display controller registers seen in
    the runs;
  - the scaler writes the picture into the first 408 lines of the 720×480 buffer, and the buffer is shown 1:1
    from line 37;
  - lines 443–479 of the buffer fall off the panel. They are outside the picture, so clipping the work area to
    the panel loses nothing.
- Proposed (not yet in the shared build): clip the display work area to the panel size, with a unit test.

### M7 — movie playback: what the progress bar and time counter show, and why they stalled
- Read from the 5.11 playback code: the progress bar and the time counter follow the number of the frame
  actually shown. They show frame × 100 / total frames, and a per‑frame timecode stored with the clip.
- The player shows a decoded frame only if it arrives in time for its display refresh. A frame that arrives
  late is dropped, and the shown frame number does not move.
- The decode model's default delay per picture was 37 ms (27 pictures per second). That is slower than the
  clip's 29.97 frames per second, so after the first frames every picture came too late and was dropped. The
  bar stopped near 22 % and the counter stayed at "0s".
- With a delay of 10 ms per picture:
  - all 60 frames of a 2‑second clip are shown and none is dropped;
  - the bar reaches 100 % and the counter shows "1s" (59 frames, 1.97 s);
  - the camera then returns to the playback screen.
- The same run also used the camera's real audio data rate. That rate is not read by this display path. Which
  of the two settings caused the change is likely the picture delay, but has not yet been separated by a run.
- Proposed (not yet in the shared build): a shorter default picture delay for the decode model, with a unit
  test and regression runs.

### M7 — movie playback: a fake VPU decoder replaces "READ ERROR"
- Movie playback goes through the Mali‑V61 VPU in decode mode. Without decode answers the camera shows "READ ERROR".
- Read from the 5.11 driver (unit 5): the decode messages it sends and the answers it waits for.
  - Bitstream buffers come in, frame buffers come out.
  - The sequence‑parameters answer (2015) is not parsed; the driver replies with an output flush.
  - A returned frame is accepted only once the driver's decode state has reached its third step, and only with
    non‑zero flags. A frame that comes earlier is dropped silently.
  - The step that unlocks frames runs when the driver task wakes on its event flag. It is not a timer poll.
- New opt‑in `decode` option of the Mali‑V61 model (off by default). It answers:
  - GO;
  - the sequence parameters;
  - each bitstream buffer;
  - the output flush.
  It then fills the frame sent after the flush with flat grey, `picture_ticks` later, once per bitstream marked
  "end of picture". No H.264 is decoded. One unit test.
- Result: the playback screen now shows the movie thumbnail in its film strip, with the play prompt and the clip
  format (FHD 29.97), instead of "READ ERROR". The replay is bit‑identical (reports, frames, VPU state).
- Without the option, two existing plans (card playback, 2.9 G; movie mode, 3.5 G) are bit‑identical to the
  shared build. Those plans do not declare the VPU. The "VPU present, decode off" path is covered by the unit
  test only.

### M7 — movie playback runs: one picture per slice, and a video layer cut at the panel edge
- The 5.11 driver hands each sample to the VPU as a little‑endian 32‑bit length followed by the NAL unit, with
  no start code. The decode model now finds the NAL types in either length order or after start codes.
- A picture is now owed for each bitstream buffer that holds a slice (NAL type 1 or 5). Before, it was owed only
  for a buffer marked "end of stream", so after the thumbnails the playback session got no picture at all.
  "End of stream" still flushes what is left.
- Frame formats, read from the driver: 0x201 is NV12 and 0x121 is one plane of 4 bytes per pixel. The model's
  plane order was already right.
- When a 16:9 movie plays, the firmware places the 720×480 video picture at row 37, at 1:1, on the 480‑line
  panel, so its last 37 rows fall below the panel. The display model used to stop on this as an invalid
  geometry. It now cuts the layer at the panel edge. Only geometries that used to be an error change. Whether
  the real display controller also cuts at the edge is not confirmed.
- Result: playback now runs on. The progress bar advances, and the camera returns to the playback screen when
  the clip ends. The picture is still flat grey, and the time counter stays at "0s". Every run below was
  replayed and matched bit for bit:
  - playback from the thumbnail;
  - playback under the realistic per‑core time base.
- With decode off, the card‑playback and movie‑mode plans are still bit‑identical to the shared build. That
  covers reports, frames, the VPU state, memory maps and the kernel dump.

## 2026-11-09

### M5 — still capture: where it stops, and two wrong leads closed
- After the RAW frame, the capture flow issues its IIFP (develop) request, but the develop service never runs.
  Runtime watches in counted mode (one branch ≈ 50 s) show that nothing reaches it at all.
- The develop chain starts in the `recmb` tasks, a small interpreter: each incoming code selects a list of actions,
  and only the list for code `0x1290` contains the action that forwards the develop request (`0x1291`) to the IIFP
  mailbox. During the capture, nobody posts `0x1290`; the `recmb` and `recbkfrm` tasks have run only once since boot.
- Wrong lead 1, "develop phase lock": for a normal still the phase decoder returns phase 0; the read‑back‑end
  messages that looked missing are only posted for special capture kinds (phases 1 and 2). They are skipped by design.
- Wrong lead 2, "busy queue": the request queue is filled at full press, after the half‑press decision; it means
  "capture in progress", not the cause.
- Decoded along the way: the record controller's table‑driven state machines (UI → `reccont` handshake
  `0x210`/`0x217`) and the capture‑start command `0x1158`. The JPEG encoder completion is GIC 135 (static, 90 %).
- Open: who should post `0x1290`, and with which request descriptor.

### Bench — the six efficiency cores are used too
- The Apple M5 host has 4 performance and 6 efficiency cores. The same 600 M‑instruction plan run normally and under
  `taskpolicy -b` (efficiency cores only) gives **byte‑identical reports and frames**, at about 58 % of the speed.
- `outils/paralleles.sh -e` and trunks started under `taskpolicy -b` now run non‑urgent branches there:
  4 runs on the performance cores plus up to 4 on the efficiency cores.

### M6 — the official update really rewrites firmware units in the emulator
- The update file is a 628‑byte header followed by the whole NOR image, inverted. On our emulated NOR, all 382
  blocks from 0 to `0x2FC0000` already match it, which is why earlier runs wrote no unit (not a version check).
- The official update path (card boot, update screen, confirmation, fake RL78 loader) was replayed on emulated NOR
  copies that differ from the update file. These copies are emulator‑only and never leave the bench.
  - 4 KiB changed in unit 7: only the 128 KiB block `0x9C0000` is rewritten.
  - Unit 2 entirely and the first block of unit 0 changed: exactly those 7 blocks are rewritten.
  - In both runs, the rebuilt NOR matches the update file **byte for byte** over the whole firmware area
    (`0` – `0x2FC0000`).
- So the updater compares the NOR with the update file and rewrites only the blocks that differ, in the same pass as
  the RL78 programming. It then writes the adjustment block `0x2FC0000`, data and settings, and asks the power
  controller for a restart (register `0x12` = `0x8000`).
- Wrong leads corrected: the word at header `+0x21C` is a model table, not a checksum; the update command `0x109`
  selects another kind of update (the camera then asks for another file), not "write units"; the units are not
  written after the restart.
- After the restart, the updated camera shows the language screen and, with no key pressed, powers off, exactly
  like the factory (unconfigured) NOR in the same plan. Bisection shows the cause is the update resetting the
  settings areas (`0x3E00000`, `0x3F80000`–`0x3FC0000`), not the adjustment block or the data area.
- The RL78 side uses our model of its serial loader. Nothing has been written to a real camera.

### Emulator speed — exact per-instruction fast paths and opt-in counted execution
- Profile of a real X-T3 run (`sample`): 96 % of the time was inside Unicorn's per-instruction code hook. The XP4
  board hooks every instruction, while the generic board already runs Cortex-A7 cores in counted native blocks.
- **Fast paths, always on, reports identical byte for byte** (×2.7–3.6): the vendored Unicorn restores the JIT
  write protection after a hook only when the hook changed it; instruction words that translate into DRAM are read
  from the RAM backing; a bitmap filter skips label/return/observer lookups; `due()` no longer moves the device set
  out of the run state; interval timers are walked only once a counter is due.
- **Counted execution, opt-in** (`FUJIMOD_XP4_COUNTED=1`, ×10–16): each core runs on `aprofile::Core` with two new
  owner options (exclusives and IRQ entry stay with the board). Internal runs end exactly at the next device
  deadline, MMIO accesses resynchronise guest time, IRQs, exclusives, identity reads and WFI/WFE/SEV happen at the
  same instruction boundaries as before, the exclusive-monitor store hook only watches armed monitor pages, and
  device RAM writes invalidate only the translated code they touch.
- Checked against the unmodified binary on R2 (600 M and 1.2 G), PCH (2.89 G, capture, `time_base` [40, 1]),
  K32 (2.9 G, SD card and JPEG playback) and N6 (3.3 G): reports key by key, every frame and every dump identical.
  The only difference in counted mode is the per-core trace of the last 32 instructions, which it does not keep.
  Features that need every instruction (driver guard, memory/function watches, instruction points, discovery)
  are refused in counted mode with an explicit error.
- **Coverage** (counted mode): bench action `coverage` writes the instructions entered for the first time since the
  previous one; a helper maps them to the Ghidra function index and exports EZCOV for Cartographer. On PCH, the full
  press enters 39 343 new instructions in 1 167 functions; the capture reaches the band-count gate `0x1EC18F8` but
  never its completion `0x1E9F640`.
- **Indirect edges** (counted mode): bench action `indirect` records where computed branches (`blx rN`, `bx rN`
  except `bx lr`, `ldr pc` / `ldm … pc` not from the stack, `mov`/`add pc`) actually went. A helper groups them by
  target function and writes a CSV that a Ghidra script imports as `COMPUTED_CALL` / `COMPUTED_JUMP` references, so
  callbacks registered through pointers get their callers. Example: the GIC 106/107 handlers are called from the
  IRQ dispatcher's `blx` at `0x107C3AC`, task entries from the ThreadX trampoline at `0x5584C`.
- Tests: XP4 board 12/12 (default mode); in counted mode 7/7 outside the driver-guard tests, which get the explicit
  refusal; `emulator-cpu-unicorn` 206/206. The fast paths alone are identical byte for byte, last-instruction trace
  included, on R2, PCH, K32 and N6. A branch forked from a live snapshot is identical in both modes.
- Memory and function watches also run in counted mode: data accesses through hooks on the watched ranges, instruction
  fetches through observers on the same ranges, recorder entries and returns as single-address observers. Checked
  against the per-instruction binary on P4 (860 488 memory records, 56 324 recorded calls, 6 797 edges), U18 and a
  run watching hot code (31 855 fetch records), identical.
- Details: `drafts/08-pr-xp4-counted-execution.md`.

### M6 — `ffinit.bin` runs in the emulator with an emulated boot ROM (HLE)
- No XP4 boot ROM dump exists, so the ROM is **assumed** (high‑level emulation, runs named `HLE-…`). No
  emulator code change: the XP4 board already maps physical ROM/SRAM windows (`memory_regions`).
- The emulated ROM provides only the file API that `ffinit.bin` calls (mount `0xFFF023D0`, open `0xFFF02480`,
  read `0xFFF025E4`, close `0xFFF027D8`, from the catalog's `file_api`) and a pen for cores 1–3. Every other ROM
  word is an undefined instruction.
- The card's `ffinit.bin`, loaded at `0xFFF10000`:
  - replays the update file's header operations and runs its DDR start‑up code;
  - mounts the card, opens and reads `ffboot.bin` (0x3CBF000 bytes) and closes it;
  - computes CRC‑32 `0xCBF0CC24`, the expected value, and jumps to 0 after 462.6 M instructions.
- DRAM after the read is **byte‑identical** to `ffboot.bin` (SHA‑256).
- The boot that follows matches the run with `ffboot.bin` preloaded: same 21 milestones, the five setup screens
  and three of the four playback captures are bit‑identical. The two other differences come from our setup (the
  exposure readout reads the SRAM window that covers the statistics SRAM) and from a loading indicator caught at
  another instant.
- Assumed ROM behaviour, injected and logged:
  - the ROM loads `ffinit.bin` unchanged and starts core 0 in it;
  - the file API follows the contract read in `ffinit.bin`; file bytes come from the card's file;
  - cores 1–3 wait in the ROM and jump to 0 on the first SGI the firmware sends;
  - DDR PHY channel 0 training reads as nominal (two words; the DDR code requires non‑zero results).
- Finding for real hardware: `ffinit.bin` parks cores 1–3 in a loop with no exit, while `ffboot.bin` waits for
  them (`0x7A0`) and wakes them with SGIs. The real ROM must therefore keep them outside `ffinit.bin`. To be
  checked against a ROM dump or upstream behaviour on other XP4 bodies.
- Reliability about 70 % at best without a ROM dump. No conclusion about the hardware.
- Correction: earlier notes gave the file API as `0xFFF040D0…` and the load address as `0xFFF11000`
  (decimal‑to‑hex slip). The catalog values above are right; the load address is `0xFFF10000`.

## 2026-11-08

### M5 — the firmware writes a file on the emulated SD card
- New bench action `card_dump` (every non‑zero sector of the emulated card) and a FAT32 reader
  (`outils/carte_lister.py`).
- On an empty 8 GB FAT32 card, the firmware creates `/FFDB/FFXFER.DAT` (84,048 bytes, FUJIFILM transfer
  database, dated by the camera clock) while starting up. Its directory and FAT entries are valid. The
  emulator's card builder knows nothing of this folder, so the write comes from the firmware.

### M4 — every menu page and sub‑menu is reachable in the emulator
- 200 DOWN presses from MENU walk through every tab and page (image quality, AF/MF, shooting, flash, movie,
  set‑up) and wrap around.
- Opening the first set‑up entry and scrolling on walks through all seven set‑up sub‑menus and their pages
  (user setting, sound, screen, buttons/dials, power management, save data, connection).
- No error in 13.8 G instructions. With the setting test below, the menus milestone is complete.

### M4 — a setting changed in the menu survives power‑off and applies at the next start
- In the emulator, on a configured body: MENU opens IMAGE QUALITY SETTING; image quality NORMAL → FINE; a
  half‑press returns to shooting. The screen then shows "S F" and the remaining‑shot count drops from 3153
  to 2145.
- The POWER switch set to OFF runs the clean power‑off: 160 NOR pages and a new settings header in the
  power controller.
- Restarting from that NOR and header, with no key pressed, brings back "S F" and 2145 shots.
- `outils/nor_apres_run.py` rebuilds the NOR after a run and prints the header registers the firmware wrote.
  Emulator use only.

### M6 — the emulated update programs the power MCU with exactly the shipped image, then writes the NOR
- With the RL78 loader on UART 0 (two wires, code flash end 0xBFFF), the official update runs the whole
  serial‑programming sequence: Baud Rate Set, Reset, Silicon Signature, then 48 × (Block Blank Check,
  Programming) and 48 × Verify. That is 384 data frames, no sum error and no erase, since the emulated flash
  starts blank.
- **The emulated RL78 flash equals the update's 48 KiB image byte for byte** (48/48 blocks). The image sits
  in the update file at 628 + the slot start, inverted. With code flash end 0x3FFF the updater takes the
  16 KiB image instead, also 16/16 identical. The check is replayable (`outils/rl78_comparer.py`; it prints
  counts and digests only).
- With an echo on a single wire, the update fails at the first command: the real wiring is two wires.
- **Anti‑brick, confirmed in emulation**: an update file whose four RL78 slots are empty (10 header bytes
  changed, a private test file) is accepted. The updater skips the programmer, never opens UART 0 and goes on
  with the update.
- In all three runs the update then **writes the internal NOR**: 379 pages of 256 bytes by 6.6 G instructions,
  in the 128 KiB blocks at 0x2FC0000, 0x3E00000, 0x3F80000 and 0x3FA0000 (the settings journal). The 70 KiB at
  0x3E00000 are new data whose role is not identified yet. Longer runs will show whether the update completes.

### M5 — first RAW frame from the emulated sensor; core 3 and the movie mode
- The XP4 board wired the sensor receiver without read‑out channels, so a requested still read‑out never ran.
  With the fix, the electronic‑shutter capture gets its frame: 6276 × 4212 RAW samples written to the buffer
  the firmware programmed. The JPEG encoder is not started yet: the next wait after the read‑out is still to
  be found.
- In movie mode, core 3 stopped for good about 0.55 G instructions after the mode change. Its DMA‑completion
  poll (`0x111bf78`, about ten tries per audio block) timed out, and with no error callback registered the
  status handler (`0x111bac8`) snapshots the DMA channels and loops on purpose. The slow time base made it
  happen sooner, but at 40 instructions per tick it still happens, after 57 good polls. The DMA model now
  reports the address a linked channel has reached mid‑block, as hardware does, but that changes nothing.
  The error comes from a message core 3 sends at the end of each audio segment (`0x111a3e0` →
  `0x1134dd0`, message buffer 0x32). The 58th send fails, which suggests a full buffer whose reader never
  runs. Finding that reader is the next step.

### Secondary chips of the X‑T3: static inventory (anti‑brick)
- **Power sub‑MCU, Renesas RL78**: the only chip besides the NOR that a body update reflashes. Two copies of its
  program ship in FWUP0019, at the end of the payload: one for 16 KiB of code flash and one for 48 KiB. They are
  selected by the slots of the update header, which sit 8 bytes into the update structure.
  - Protocol A has no read command, so this flash cannot be backed up. It can only be checked against the
    shipped image with the Checksum command.
  - With all four slots empty, the updater skips the programmer and reports success. This is confirmed by two
    independent static readings, and since by an emulator run (see above). The "prepared" flag at `struct+0x994` is not a way to skip it:
    it only triggers payload preparation before the RL78 step.
- **Wi‑Fi / Bluetooth**: a Broadcom BCM43438‑class module (Wi‑Fi `43430a1`, SDIO, DHD driver). Its firmware and
  Bluetooth patch are loaded into RAM from the NOR at every boot, so there is nothing to brick.
- **I²C parts** (F_I2C, channels 0–2, device table `0x13e5a18`):
  - accelerometer for the electronic level at 0x1C / 0x1D: writes `CTRL_REG1` (0x2A), so NXP MMA845x‑like;
  - fuel gauge at 0x36: model unlock 0x4A 0x57 written to 0x3E, so Maxim MAX1704x‑like;
  - eye sensor at 0x44 / 0x39;
  - a factory‑calibrated part at 0x1B / 0x33, which receives 22 adjustment values at start‑up.
  None of them gets firmware from the body update.
- **A second programmer** (`0x1debabc`) speaks a Renesas boot‑mode protocol at 19 200 baud: sync 00 / 55,
  answer E6, commands 20, 10, 21, 11, 3F, 40, 48, 58, 43, 50, 52, 4D. It uses the lens‑communication driver and
  validates a 0x200‑byte image header. It is most likely the lens or accessory updater, not used by a body
  update (hypothesis).

### M6 — the body update reprograms the power sub‑MCU (Renesas RL78) over UART 0
- Serial port interrupts are found: the handler table at `0x13e5870` maps port 0 to sources `0x28`–`0x2b`, which
  are GIC 253–256. An emulator run confirms them; the earlier candidate, GIC 92, was wrong.
- Serial resource 5 is port 0 at 115 200 baud, driven by GPIO 10.5 / 10.6; its client is in the power driver.
  `0x10a49dc` sends RL78 "Protocol A" serial‑programming commands: 0x9A, 0x32, 0xC0, 0x22, 0x40, 0x13.
- The frame seen in emulation, `01 03 9A 00 21`, is Baud Rate Set (115 200 bps, 3.3 V); the full frame ends with
  `42 03`. The replies of a fake bootloader come from Renesas application note R01AN0815.
- **Anti‑brick note**: the official update also rewrites the power MCU's own flash. A full backup of the camera's
  NOR does not cover it.
- The emulator now has an RL78 flash loader to put at the far end of port 0. It handles the frames (sums checked)
  and the commands the update uses (0x9A, 0x32, 0xC0, 0x22, 0x40, 0x13), plus Reset and Checksum, and keeps a
  programmed code flash so verify and checksum agree with what was written. Its answers were checked against
  R01AN0815 rev. 1.10.
- With port 0 on GIC 253 the update sends the whole frame, `01 03 9A 00 21 42 03`. It reads what comes back and
  retries (12 times in 1.1 G instructions) when the answer is not an RL78 status frame.
- The update checks the silicon signature before any erase. It compares the code flash end + 1 with the sizes
  of the image slots in the update header: one 16 KiB image and one 48 KiB image in FWUP0019, the other two
  slots empty. When no size matches it stops with an error and erases nothing. The firmware version field is
  not compared. Static reading suggests an update file with all four slots empty skips the power‑MCU programmer
  entirely; this is not yet confirmed by a run.

### M5 — electronic‑shutter capture: the shutter sound goes out through DMA
- With the sensor read‑out start latched, the still capture (electronic shutter) arms channel 0 as expected
  (`0x1dcb570` → `0x1049910` at 2.8095 G instructions).
- About 15 ms of emulated time later the firmware plays the shutter sound. DMA channel 7 sends 5 760‑byte
  blocks, address incremented, to the fixed address `0xfff4f004` (audio port 0 transmit). The emulator refused
  fixed‑destination DMA. It now drops the bytes at the audio pace.
- Movie mode (H.264 FHD 24p, microphone absent): no error. The audio capture DMA runs (35 blocks of 30 ms), but
  pressing the shutter button does not start recording within 1.7 G instructions: the standby screen and its
  remaining‑time counter stay unchanged. Giving the VPU its handshake values changes nothing: none of the VPU
  driver entry points (init, session, start, send) is ever called. Recording stops before the encoder.


### Static maps for the capture and movie paths: DMA linked lists, audio ports, interrupt sources
- **DMA controller** at `0xfffe0000` + channel·`0x1000` (driver `0x107b7ac`–`0x107c290`):
  - per channel: `+0x10` count−1, `+0x14` source, `+0x18` destination;
  - `+0x1c` / `+0x20` source and destination config: bit 2 fixed address, bits 8–11 burst−1, bits 16–17 width;
  - `+0x24` = **next descriptor address | 1** (linked list);
  - `+0x28` control, bit 28 = start; `+0x30` bit 16 = busy.

  Descriptors are 32 bytes: count−1, source, destination, two configs, next | 1. The request mux is at
  `0xfff4a500`; the channel n interrupt is GIC 207 + n. The movie audio stream uses request 28, a fixed source
  and a "next descriptor" refreshed per 30 ms block.
- **Two audio serial ports**:
  - `0xfff4f000` receives (microphone) and `0xfff78000` transmits;
  - registers: `+0x00` FIFO, `+0x08` format, `+0x18` control, `+0x1c` self‑clearing reset, `+0x20` FIFO
    thresholds, `+0x28` DMA requests.
- **GIC 144 = completion of the engine at `0xfffa8000`.** `0x1090f80` enables source `0x71`, then writes `+0x04` = 1
  without waiting. The handler `0x1090e24` only runs a callback. The capture enables 144 and no model raises it.
- **The movie encoder is a message‑driven VPU** (block `0xffe00000`, GIC 138, driver `0x1097000`–`0x109e000`):
  - handshake: `+0x50` = 1, then wait for `+0x10` = `0xffff`;
  - four session slots in `+0x14`; per‑core windows at `+0x200 + 0x40·core`;
  - two message rings of `0x3fc` words in RAM; each message starts with a header word (code, plus length << 16);
  - response codes 2001–2018 (SET_OPTION, INPUT, SEQ_PARAMETERS…); the VPU can call the driver for memory
    (MEM_ALLOC / RESIZE / FREE).

  Recording a movie in the emulator will need a fake VPU. The strings (`ff_mali_VpuMain_task`, `V61_drv_init`,
  a VERSION / NCORES / JOBQUEUE / IRQVE register dump) identify an **Arm Mali‑V61**. Its message codes match Arm's
  MVE host protocol (published with Arm's GPL‑2.0 `mvx` driver as `mve_protocol_def_v2.h`), so the fake VPU can
  follow a public protocol.
- **Interrupt map**: 132 sources (table `0x13e6454`). 37 sources with a direct installation are linked to their
  handler and to the device blocks it touches, so each newly modeled block comes with its interrupt line.


### Static census of hardware polling loops
- A capstone scan finds "read a device register, test, loop back" in the exported units. Reads go through the
  firmware's MMIO helpers `0x5ac00`/`0x5ac18`/`0x5ac30` or direct loads from constant bases. Each loop is checked
  against a board definition.
- It finds the walls already hit in emulation (`0xfffa8000`, `0xfffa8004`, `0xfff7801c`, `0xfff4f01c`).
- Against the current definition: 44 distinct waits.
  - Covered: 32 (idle bits, command laws, dedicated models).
  - Likely next walls:
    - 6 undeclared blocks: `0xfff71000`, `0xfff7d000`, `0xfff82000`, `0xfffb0000`, `0xfffb9000`, `0xfffab000`.
      Each driver writes 0 at +0 (or clears bit 0), then waits for a status bit to read 1.
    - 3 plain storage blocks.
  - Two USB DWC3 waits (`DCTL.CSFTRST`, `DEPCMD.CMDACT`) are outside the capture path.
- Second pass: the census now follows register accessors (small functions that read one register and return it
  or store it through a pointer; 132 found). It also resolves driver base pointers from real RAM dumps.
  - It recovers a wall missed before (`0xfff6c400`).
  - 9 more likely walls, among them a two‑instance block at `0xff604000` (+0xdc must read `0x300`). The JPEG
    driver waits on it, so it is a candidate for the encode step.


### M6 — clean power‑off works in the emulator
- The power switch is bit 10 of word 0 of the power controller, which the firmware samples over SPI 1
  (`0x8080` = ON, `0x8480` = OFF). It is not a GPIO pin. A new bench action `power_input` moves that word mid‑run.
- Two hardware acknowledgements were missing:
  - the sensor stand‑by bit at `0xfff6c400`: the firmware writes 0, then waits for bit 0 to read 1;
  - the table‑load start bit at `0xfffa8004`: the firmware writes 1, then waits for bit 0 to clear.
  Both are declared as command laws of plain register blocks; no code change was needed.
- With them, switching OFF from the shooting screen runs the whole shutdown:
  - the "Sensor cleaning" screen appears;
  - 160 pages of settings are written to the NOR;
  - the settings header is written back to the power controller;
  - the cut‑power routine runs, then the firmware parks in an endless loop.
- **The off → on cycle works.** Rebooting with no key press from the NOR and settings header that this shutdown
  left reaches the shooting screen at about 1.0 G instructions. There is no first‑start screen, and the camera keeps
  its settings: M, 1/1000 (the firmware restores the dial position), ES, JPEG S NORMAL, 3153 shots left.
- Boot time also dropped. A NOR prepared with the settings journal resealed, plus the clock and settings header the
  power controller keeps, starts straight on the shooting screen at 0.9 G instructions with no key press. That is
  about 4.5 minutes instead of 32.

### M7 — X‑T3 register maps in the FujiMod catalog format
- The public native boot (`crates/emulator-boot`) only covers the X70. XP4 boards run through the Python binding
  `Board.from_xp4(options)`, whose `options` is the `emulator-xp4-board` `Definition`, the same one as our bench.
  Upstream derives it from `catalog/soc/xp4/soc.json` → `hardware.profiles.xp4.register_maps`.
- Generated from the strict board definition (T4) and the #184 SoC file:
  - 71 `register_blocks` (9 with `idle_bits`, 12 with `commands`);
  - `jpeg_decoder` and `jpeg_encoder`;
  - `dram_alias`;
  - `serial_ports`.

  The candidate `soc.json` deserializes with `fujimod_catalog::reference::Soc` and passes `Soc::validate("xp4")`.
- The schema cannot express yet:
  - a standard SDHCI host;
  - the Socionext F_I2C controller;
  - the 16‑bit timers;
  - job blocks whose acknowledge or status lives elsewhere;
  - imaging options;
  - power‑controller inputs 48–55 and the RTC.


### Configured-body boot: where X‑T3 5.11 keeps its settings
- Fact locators (`ui_modes`, `menu_cursor`, `usb_normal_ids`…) are not in `crates/corpus`, which is only the search
  library. They come from a private upstream generator, so only upstream can move the facts counter. In the
  public schema, `identity_checks` are code ranges `{address, size, sha256}`, not the IOP code.
- Generic setting reader: `0xf210b8` → `0xff55ac` → `0xfe7988`, a jump table on the `UI_SETP_*` id. Most values
  are one byte of a live settings structure at `0x181C3A0` (`0x83D8` bytes per set, 7 sets). Located:
  - `UI_SETP_LANGUAGE` (0x58) at +469;
  - `UI_SETP_UNMOUNT_SHUTTER` ("shoot without lens", 236) at +0x2F7;
  - `UI_SETP_NOCARD_SHUTTER` (237) at +0x2F8;
  - `UI_SETP_SHUTTER_TYPE` (0x452) at +0x436.
- The settings journal (RAM `0x168E20C`, the PR #184 profile) is an image of that structure, offset by `0xF4`.
  It changes only at power‑off. Its two NOR copies are at `0x3FA0000` and `0x3FA8000`, sealed like upstream
  `seal_journal` (verified on both copies).
- The first‑start state is not in NOR. A 16‑byte settings header (RAM `0x18D28B0`; byte 12 = first‑start
  bits, byte 14 = `0xAA`, byte 15 = checksum) is pushed by `0x1124598` → `0x10a33e8` into power‑controller
  registers 48–55. Our model restarted them at 0, so a seeded NOR alone reopened first start (language screen).
- The power register model now accepts optional initial values for registers 48–55. With the NOR pages of an
  earlier run plus its registers 48–55, the camera skips the language screen and stops on the area screen.
- Journal writer `0x11244b4`: record `[4, 0x104)` ← `0x18DA394` (256 bytes); record `[0x104, 0x104+0x7AD4)` ←
  `0x18D28C0` (the bytes after the header, equal to the live structure + 0x10); then version bytes at
  `+0x7BF8`. Records `[0, 4)` hold the copy number. Copying the live structure over the version made the
  firmware reject the journal (back to the language screen).
- **Booting a configured body with no key presses works.** The run uses:
  - a NOR copy whose journal is rewritten like the firmware writer would, from an earlier run's memory dump,
    with `UNMOUNT_SHUTTER = 0` and `SHUTTER_TYPE = 1` (ES);
  - power registers 48–55 set to a finished‑first‑start header;
  - the power‑controller clock running.

  At 0.9 G the camera shows the shooting screen in French with ES and the usual battery warning. Getting there
  took about 10 G instructions of menu presses before. ES = 1 was confirmed independently at the end of a
  capture branch.
- Image and movie settings in the live structure:
  - `PIXEL` (+656: 0 L, 2 S);
  - `ASPECT` (+657);
  - `QUALITY` (+658: 0 FINE, 1 NORMAL);
  - `MOVIE_PIXEL` (+659: 2 FHD);
  - `MOVIE_FPS` (+662: 1 24p);
  - `MOVIE_BITRATE` (+664: 3 50 Mbit/s);
  - `MOVIE_CODEC` (+665: 1 H.264).

  A configured body with JPEG size S and NORMAL quality boots showing "S N" and 3153 shots left (545 at L FINE).
- New tool `nor_reglee.py`: it copies a report's NOR pages onto a copy of the base NOR. It can also write named
  settings into both journal copies and reseal them. The image is for the emulator only.


### M6 — the emulated update runs until it fails for lack of an answer on a serial port
- From the update screen, the bench walks the official dialog with the keys: OK, "Body", OK, Execute
  (UP then OK), the confirmation screen, then OK. The update starts and shows "Processing" with a 10‑box progress
  bar.
- About 5.87 G instructions in, the update program opens UART 0 of three ARM PL011 UARTs at
  `0xfff39000 + 0x1000·n`. FujiMod had no PL011 model, so the run stopped there.
- **New model `emulator-xp4-io/src/pl011.rs`.** An idle port: the transmitter drains at once and the receiver
  stays empty. Line settings are kept, and sent bytes are kept for the report.
- With the model, the update goes further and then fails:
  - the first box completes;
  - "Reprocessing" (a retry) appears near 6.0 G;
  - from 6.25 G, a fixed "Firmware update error — contact a service center" screen stays up to 12 G.
- No unmapped access, and no NOR write beyond the usual settings pages. The likely cause is an answer the
  update waits for and does not get, because nothing is attached to the ports. This is a hypothesis.
- The three ports shared one device name, so the report kept only one of them. Each port now has its own name.
  A run with port 0 alone is reading the sent bytes.
- Nothing here touches a camera. The emulated NOR is an overlay on a dump.

### M5 — at a realistic rate, the still capture gets past the core‑3 handshake
- The menus run at the old rate, then the time base switches before S1 to either:
  - `[40, 1]` over all cores;
  - `per_core_time` with `[10, 1]`.
- With either setting, core 3 runs threads again, and the photo thread's handshake completes.
- Both runs then stop about 0.15 G instructions after S2 on an unmapped read at `0xfff1e000`. Core 1 copies
  `0xd800` bytes from the second statistics bank (`0xfff17000`) to RAM. Our statistics SRAM model holds only two
  `0x7000` banks, so the real SRAM is larger: at least up to `0xfff24800`.
- New opt‑in `imaging_statistics_window` widens the SRAM. Captures with a 128 KiB window are running.
- The start‑up card fix: `Preload.program_address` and an exclusion check in the recovery recipe. The rebuilt
  `ffboot.bin` is byte‑identical to the hand‑relocated copy that boots in the emulator.

### M6 — the recovery card runs the camera from the card image
- With only `ffinit.bin` and `ffboot.bin` on the card (no update file), `ffboot.bin` preloaded at RAM 0 and no
  key held, the camera behaves as after a NOR boot. Over the same plan (setup screens, then playback of a test
  JPEG):
  - the five setup screens are bit‑identical;
  - the photo is displayed identically;
  - the JPEG decoder runs the same 4 jobs, and the same 2,030,592 card bytes are read.
- Only two differences: 541 shots left instead of 545, because the card holds 64 MB more, and a loading
  indicator caught at another instant.
- Limit: `ffinit.bin` itself does not run (no boot ROM model), so its load and CRC check are not covered.
- The startup card's reader can be moved. It holds 13 absolute internal addresses and no outside branch; 6 hooks in
  the image branch to it. A relocation tool works on a copy, and an emulated boot of the relocated copy is running.
- `memory_watch` (already in `emulator-xp4-board`) is now reachable from the bench, to prove a RAM area stays
  unused: every CPU access is recorded, and a final dump catches DMA writes.

### M6 — the update screen is reached; the X‑T3 startup card built by FujiMod cannot boot
- **The official firmware update screen appears.** Two runs reach it, with `FWUP0019.DAT` on the virtual card
  and DISP/BACK held from power‑on (ADC channel 3 = 2311):
  - the stock NOR boot;
  - the FujiMod recovery card (`ffboot.bin` preloaded at RAM 0, as `ffinit.bin` would after its length and
    CRC‑32 checks).
  The version boxes stay empty. Nothing was confirmed on that screen.
- **First wall: the virtual card's sparse bound.** A 64 MB `ffboot.bin` plus a 15 MB `ffdata.bin` exceeded the
  65 536‑sector bound of our card definition ("SD sparse allocation bound exceeded"). This is the wall of X70
  issue #46. Upstream uses `1 << 20` sectors (X70) or `1 << 21` (XPP); our plans now use `1 << 20`.
- **The startup card (`ffinit.bin` + `ffboot.bin` + `ffdata.bin`) crashes on the X‑T3 5.11.** Bisected in the
  emulator:
  - The recipe redirects one initialization call (`0x1147c8c`) to its preloaded `ffdata.bin` reader, placed at
    the fixed address `0x1a0000` (0x678 bytes). That address comes from the common XP4 SD‑loader asset, which has
    booted X‑S10, X100V and X‑Pro3 cameras.
  - About 7.8 M instructions after reset, the X‑T3 kernel sets up five 128 KiB buffers at `0x124000`,
    `0x144004`, `0x164008`, `0x18400c` and `0x1a4010` (header `0x1234`, then zero fill). The fourth one covers
    `0x18400c`–`0x1a3fcc` and wipes the reader.
  - About 41 M instructions after reset, the redirected call runs into zeros and ends in an abort at `0x1a400c`.
  - The recipe checks that the reservation is empty in the image and outside the declared BSS. These buffers are
    neither: they are filled at run time.
  - The recovery card does not install the reader, keeps the stock call, and is not affected.

### M5 — the still capture starves core 3; the time base can now change mid‑run
- **A traced capture (`[4, 1]`) shows that core 3 never runs a thread after S2.** Over 1 G instructions:
  - the photo thread spins on core 1 on a byte at `0x024a9310`;
  - the function that should set that byte is queued for core 3;
  - all 50 samples of core 3 are in IRQ or SYS mode, the interrupt context of this ThreadX port. Threads run in
    SVC mode.
- Each soft‑timer mailbox post on core 3 takes 0.4–1.8 M instructions, waiting for the kernel's SMP lock.
- `[4, 1]` gives about 72 MHz per busy core against 54 MHz timers. The firmware's own timer settings point to
  about 540 MHz. The periodic timer work then fills core 3.
- `wfe` does end the core's slice until the next `sev` or interrupt: the spin itself is not the problem.
- **New:** `Session::set_time_base` (`emulator-xp4-board`, test `the_time_base_can_change_between_slices`,
  board 12/12) and a `time_base` bench action. Menus can be driven at the old rate, then the capture runs at a
  calibrated rate (`[40, 1]` over all cores, or `per_core_time` with `[10, 1]`).
- `sample` actions now also record per‑core instruction counts, ticks and interrupt entries.

### Keys through the analog ladder work: the freezes came from the board's time base
- **The X‑T3 key ladder (ADC channel 3) now drives the interface.** Each DOWN or UP press moves the cursor in
  the language list. LEFT no longer freezes the key task.
- The cause was the time base of the XP4 board model, not a missing device:
  - guest time advanced one tick per emulated instruction, counted over all four cores together
    (`instructions_per_tick` defaults to `(1, 1)`);
  - with 54 MHz timers, each core therefore got about 13.5 M instructions per simulated second, far below the
    real processor;
  - the periodic work on core 3 (soft timers, the key task, the timer task) then used the whole core: mailboxes
    filled up (−62 errors) and the key task, although ready, never ran again.
- The freeze moved with the scheduling quantum: 20 000 froze after MENU, 40 000 did not, and 5 000 or
  10 000 did not boot. This matches a known issue class: QEMU icount in SMP shared one instruction budget across
  cores until it switched to a per‑core budget.
- With `instructions_per_tick = [20, 1]` (definition field, no code change), the key scanner keeps running and
  every analog press acts. Boot takes more instructions, so key plans must be re‑timed.
- A calibrated value, from the firmware's own timer and tick settings, is still to be set. A cleaner long‑term
  fix would keep a local time per core.

### M5 — still capture and keys: the same wall on core 3
- **With the timers modeled, the ES capture gets two messages further.** Timer 7 fires and its callback posts
  `0x15c7`. At the next frame start (GIC 102), the photo module posts `0x153a`, and the capture moves on with
  `0x15c8`.
- **Then the photo thread waits on core 3.** It asks for a function to run on core 3 through a per‑core
  "execcpu" task: mailbox, message `0x17ef`, caller `0x1128460`. It then spins on one byte until that function
  sets it (`0x1f40bf4`), and never yields core 1.
  - The execcpu task gets the request on core 3, limits itself to core 3, then changes its own priority from 2
    to 3. That call never returns, so the function never runs.
  - From then on, many mailbox sends fail with −62 (mailbox full): the tasks on core 3 no longer drain their
    mailboxes.
- **The key freeze has the same signature.** The key scanner (`0x10c65ac`, a cyclic handler on core 3) stops
  right after MENU in some runs and after LEFT in others, with the same −62 errors.
- In every frozen run, core 3 ends inside the kernel's SMP lock (`_tx_thread_smp_protect/unprotect`) with
  interrupts masked.
- Hypothesis, not proven: one thread keeps core 3 busy, or the SMP scheduler no longer switches on core 3.
- New bench action `sample` in `xp4_run`: it records each core's pc, lr, sp, r0 and cpsr at a given
  instruction. Many of them make a coarse sampling profiler.

### M5 — playback picture works
- **The camera shows a photo in playback.** A JPEG test chart on the virtual card is read, decoded, resized
  and displayed, with its Exif data drawn over it.
- Resize engine model: new `resize` option of `job_ack`. When a job ends, it scales a packed 4:2:2 picture
  (Y0 Y1 Cb Cr) from the source window to the output window.
  - X‑T3 `0xfff7e000` registers: source and output sizes `+0x0c`/`+0x18` (w−1 | (h−1)<<16); ratios
    `+0x1c`/`+0x20` in Q12, where bit 16 halves first; source address and pitch `+0xc4`/`+0xd0`; output
    `+0xd8`/`+0xe4`.
  - Nearest neighbour: the engine's filter coefficients are not used, a declared approximation.
  - In playback, job 6 of 12 scales the decoded 1920×1280 picture straight into the displayed video plane
    (720×480, pitch 1472). The other jobs make the side‑by‑side copies used for scrolling, and a 1152×768
    version. Tests: job_ack 3/3.
- Checked: the displayed bars are in the same order as the source chart decoded on the host with `djpeg`.

### M5 — playback: engines after the decoder
- Playback goes past three more walls after the JPEG decoder:
  - `0xfa400000` (busy page): `0x1079000` waits for `+0x518` bit 1 to clear;
  - `0xfa000000`: an engine with a ready bit (`+0x00` bit 16) and a start bit (bit 0). It ends on **GIC 145**
    (level), and its handler acknowledges through bit 3 of `0xfff4aa10`, then waits for the start bit to clear;
  - `0xfff7e000` and `0xfff7f000`: twin engines missing from the catalog. Driver `0x1091674…`: clear bit 28 of
    `+0x100/+0x104` (out of reset), wait for the status word (`+0x10c`, twin `+0x0e4`) to equal `0x01000100`,
    write `+0x000 = 0` and wait for bit 0, start with `+0x004 |= 1`. Sizes are 14‑bit fields. Interrupts from
    the firmware's source table (`0x13e6454`, rows 9 and 10): **GIC 132** and **GIC 133**, level. The handlers
    acknowledge by writing 1 to the status word.
- New model `emulator-xpp/src/job_ack.rs`: a register page whose job is started by a bit and acknowledged
  either through a word of another block or through a write‑one‑to‑clear status word. What the engines compute
  is not modeled. Board field `job_ack`. Tests: job_ack 2/2, board 11/11.
- With `0xfa000000` modeled, one job completes and is acknowledged. The next wall was `0xfff7e100`.
- **With both twin engines modeled, playback reaches its screen with no wall** (card icon, play icon) and shows
  **"READ ERROR"**. The hardware chain runs, but the firmware rejects the file. Fujifilm manuals use this
  message for a file that is corrupt or was not made by the camera, and the test chart made with `cjpeg` has no
  Exif.
- What the 5.11 Exif reader checks, from its decompiled code (`0x103afd8`, `0x10404c4`, `0x1038b80`):
  - APP1 "Exif" right after SOI, and a TIFF header;
  - IFD0 with the Exif pointer (`0x8769`), orientation, software and date;
  - an IFD1 holding a JPEG thumbnail that starts with SOI and reaches a SOF0;
  - an optional maker note: "FUJIFILM", then offset 12.
- New tool `jpeg_exif.py` adds such an Exif block to a test image: FUJIFILM / X‑T3, Exif 2.31, maker note
  version "0130", 160×120 thumbnail. It contains no firmware bytes.
- Neither Exif nor sampling is the cause: with an X‑T3‑style Exif, and with 4:2:2 sampling and restart
  markers, the camera still shows "READ ERROR". The Exif reader, the JPEG decoder (4 runs on the whole file)
  and the `0xfff7e000` engine all succeed.
- Where the message comes from: the playback application lives in **unit 7**, the overlay loaded from flash.
  - `0x3a491bc` turns an internal playback code (`0x335e554`) into a UI message. Any code missing from its
    lists gives `0x900b` (`ERR_READ_ERROR`) and the debug text "PLAY:IMAGE ERR".
  - A targeted call trace shows the code is `0x90`, with the flag set. It comes from the decode state
    machine's error handler (`0x3a511b4`), reached once per decode attempt. The playback library never
    received the "decode done" message.
- **Cause: the wrong interrupt line.** The unit‑6 decode client (`0x1db95e4`) installs its completion handler
  `0x1db94ac` on interrupt source `0x66` = **GIC 136** (level), not GIC 144. The decoder model raised 144, so
  each decode finished unnoticed, timed out and was retried 4 times before the error. Source `0x71` (GIC 144)
  belongs to another path (`0x1090f80`). The handler reads status `+0x004` and acknowledges it. Bit 4 (done)
  makes it read `+0x014` and the output end `+0x108`; bit 17 means failure.
- **With the decoder on GIC 136, playback shows the photo's screen** with the data from its Exif: date, file
  100‑0001, 1920×1280, 1/125 s, f/5.6, ±0, ISO 200. A memory dump shows the whole test chart decoded in RAM.
  The picture itself is still plain green (a zeroed YCbCr buffer). The resize engine `0xfff7e000` (here
  1920×1280 → 1152×768, Q12 ratios, filter coefficients, three groups of two DMA channels) and `0xfa000000`
  run but compute nothing yet.
- `job_ack`: each page's name now includes its base, so several pages stay apart in reports. It also keeps the
  words of its first 32 job starts.

### M5 — still capture: the missing hardware timers
- With ES, the capture task arms hardware timer **channel 7** (firmware timer table `0x13e4fe4`, entry 9) and
  waits for its one‑shot callback (`0x212dcc0`, next to the code that posts message `0x15c7`). The callback
  never ran.
- The firmware's 16 hardware timers live at **`0xfff3c000`**. The kernel driver (`0x59594…`) shows the layout:
  - run word `+0x000`, where bit 16+n enables the write of bit n;
  - per‑channel control at `+0x10 + 4n`: one‑shot bit 4, expiry flag bit 6;
  - count and reload words at `+0x100 + 16n`;
  - 54 MHz ticks; channel n interrupts on **GIC 41 + n** (edge).

  The bring‑up had declared this page as a plain register file, so none of these timers ever counted. (The
  modeled interval timer at `0xfff3c300`, GIC 57–64, is a different block.)
- New model `emulator-xp4-io/src/timers16.rs` and board field `timers16`. Tests: timers16 1/1, board 11/11.

### M5 — still capture: electronic shutter
- Menu input: after the LEFT key, the firmware stops reading the key queue (the report's slot numbers keep
  growing). UP, DOWN, RIGHT, OK and BACK are fine.
- SHOOTING SETTING › SHUTTER TYPE is reached with DOWN only (46 presses from the menu's first tab). Choosing
  **ES** (electronic shutter) works: the shooting screen shows "ES", and a half press locks AF (green frame).
- The metering sees only a uniform grey (luma 2048, black level 1024) and picks a 30 s exposure in aperture
  priority. The capture then sits in its 30 s exposure. Constant statistics drive auto exposure to one end of its
  range, so the next runs use a brighter scene.
- JPEG encoder interrupt: the unit‑6 encoder client installs source `0xb` = **GIC 135** (the decoder is
  GIC 136).
- Shutter speed dial: its positions arrive as physical keys read on ADC channel 6 (95 = A, 96 = 1/8000 …
  99 = 1/1000 … 101 = 1/250; catalog `control-ids.generated.json`). Injected through the key queue on the
  shooting screen, they put the camera in **M, 1/1000 s, ES**. Injected while a menu window is open, they block
  the key task, as LEFT does.
- New bench action `scene` (`Session::set_scene`): replaces the scene the imaging statistics draw, mid‑run.
- With ES at 1/1000, a full press starts the capture and the display stops, but nothing is encoded or written.
  The capture state machine (unit 6, `0x212c…`) posts messages `0x15c3`, `0x15c4` and `0x15c6`, then waits
  forever on its queues. With ES, the frame interrupt (GIC 101) fires only once, where the mechanical path
  counted frames; the sensor is reprogrammed for a still readout that the models do not produce yet.

### M5 — still capture: waiting for the shutter
- Full runs to 6.0 G: a half press shows the AF frame with a full battery, and a full press greys the screen.
  The frame counter runs from 5.22 G on, one call per frame.
- A run to 8.5 G stays grey and writes nothing to the card, so the capture is stuck.
- Correction: `0x11a3294(n, mode)` is not an event flag. It looks up a pin in table `0x149d374` (signal 8 =
  pin 4.5, 9 = 4.4, 10–13 = 4.0–4.3, 1–7 = 9.8–9.14) and writes that pin's 2‑bit mode field (`0x10714c0`).
  The frame handler toggles pin 4.5 once per frame, which looks like a sensor sync line.
- Unit 6 names the next wait: `WAITSHUT` (posts message `0x15d5`, then waits), `not_Recive_S2_COMP_SHUT`,
  `MSHUTO`/`MSHUTC`, and the error `ERR_FOCAL_PLANE_SHUTTER_ERROR`. The capture is very likely waiting for
  the mechanical shutter, as on the X‑T20 (FujiMod #71, #77, #113), and nothing here models it yet.
- Two ways forward:
  - the electronic shutter, a normal X‑T3 setting (`MODE_SHUTTER_ELE`) with no moving parts. A run explores
    the shooting‑setting menu to find it;
  - a mechanical shutter model in the style of `emulator-xpp/src/shutter.rs`.
- Ruled out: `0xfff7b000` is the **HDMI controller**. Its state table sits next to the strings
  "HDMI DEBUG (BASE ADDR:%08X)", `HD_DETTO`, `HDSCRE1`… The loop seen every ~90 M instructions is periodic cable
  detection: GPIO 6.3 low for 20 ms, then up to 101 reads of `0xfff4a200` bit 0 (hot‑plug detect). Reading 0
  means no cable, which is correct. Its interrupt is GIC 249.

### M5 — playback: JPEG decoder
- With a JPEG test chart in `/DCIM/100_FUJI/`, PLAY reaches `0xfffa1000`. The firmware writes 1 to `+0x118`
  and waits for status bit 16, exactly what FujiMod's X‑Processor Pro decoder model describes. The X‑T3's
  JPEG encoder is at `0xfffa0000` and its decoder at `0xfffa1000`; their register sets match the X‑T20
  models. The decoder interrupt is GIC 144.
- New XP4 board options `jpeg_decoder` / `jpeg_encoder` reuse the X‑Processor Pro models. With the decoder
  wired, the camera reads the file from the card and decodes it three times (thumbnail, preview, picture).
  It then reaches a new clock‑gate page, `0xff482000`, declared as a register file with its evidence.
- NP‑W126S warning: the battery is identified correctly (`0xffabb8(0)` = 2). Each return to the shooting
  screen toggles a flag (`0x1cd30d8`), and the warning is decided in the handler of event `0x93e`, probably
  from a setting left at its factory value. One key press dismisses it.

## 2026-11-07

### Battery level through a new F_I2C model
- The X‑T3's I²C controllers (`0xfff3d000 + 0x1000·n`, GIC 65/66/67) are Socionext/Fujitsu **F_I2C**, the
  controller Linux drives as `i2c-synquacer`: byte registers at a four‑byte stride, BCR bits INT, INTE, GCAA,
  ACK, MSS, SCC, BEIE, BER. They are not DesignWare.
- New model `emulator-ip/src/socionext/fi2c.rs` (master: start, repeated start, one INT per byte, ACK/LRB,
  stop) and board field `fi2c`. Its targets reuse the X‑Processor Pro register‑file parts. Tests: fi2c 2/2,
  board 11/11.
- Fuel gauge at `0x36` on channel 1 (device 10 of the driver's table): the firmware loads a custom model into
  it and reads register 4. The battery icon follows the declared value: full with `0x5440`, low with
  `0x2000`. Power register 4 = 650 (battery present, not external power).
- `xp4_run` now merges `card_files` into the virtual SD card. First playback test with a JPEG test chart in
  `/DCIM/100_FUJI/`.

### M5 — still capture: frame counter found
- After a full press, the capture task arms GIC 101 with a frame‑counting handler (`0x20cfbd8` →
  `0x11a25ac(2, 0x20d0134)`). After N pulses, `0x20cfa24` signals event 8 to the capture task. The firmware's
  imaging interrupt table (`0x149d2d4`) lists three sensor lines: GIC 100, 102 and 101. The sensor block
  enables three causes (`+0x140 = 7`), but FujiMod's model only routed two (start/end of frame) to 100/102.
- New opt‑in `ImageRoutes.start_pulse` (sensor model) and board field `imaging_start_pulse`. X‑T3 uses
  `(2, 101)`: one pulse per frame start while cause 1 is enabled. With it, the capture advances one step, then
  waits again. Tests: sensor 12/12, board 11/11.
- Battery: power‑controller register 4 above 752 (parameter `0x3eb`) means external power (plug icon), and
  201–752 means a battery. With 650 the battery icon appears, but empty: the level comes from an I²C gauge,
  and the XP4 I²C controller is not modeled yet. The "use NP‑W126S" warning still shows at start‑up.
- In full runs, the battery warning comes back when the menu closes and swallows the next shutter press.
  Close it first (one S1 press).

### M5 — the shutter fires
- With SHOOT WITHOUT LENS set to ON through the menus, a half press shows the AF frame. A full press **starts
  a capture**: the OSD disappears and the screen turns uniform grey, as on a real body during exposure.
- The capture does not complete yet, and no file is written. During the capture the firmware enables GIC 47,
  117, 208 and 249. GIC 249 belongs to block `0xfff7b000`, which is still a plain register file here (later
  identified as the HDMI controller, not part of the capture: see 2026‑11‑08).
- Battery gauge: a call trace shows it is only polled periodically (commands 0 and 8), so it is not the
  blocker. A model was written anyway, because the update path (M6) will need it:
  - `emulator-xp4-io/src/gauge_serial.rs`: X‑T3 frames (even command, bit 0 = write, two's‑complement
    checksum);
  - `spi::SharedBus`: several parts on one SPI controller, each behind its own chip select;
  - board field `gauge_spi`.

  Tests: xp4‑io 28/28, board 11/11.

### M5 — playback mode reached, "shoot without lens" found
- **Playback works up to the empty‑card screen.** PLAY now enters playback mode and shows "1 NO IMAGE", the
  correct result for an empty card. Two memory fixes were needed:
  - memory‑controller command laws `+0x080 = 0x100/0x200` → `+0x090` (see below);
  - memory PHY (`0xff614000/24/34`): `0x108ee74` waits for `(+0x0dc & 0x1300) == 0x300` and the same at
    `+0x2dc`, now declared as idle bits.
- Discovery mode cannot find these waits: the driver polls with interrupts masked, and no probe is made. Those
  waits come from reading the driver.
- Menu navigation: UP from the first item of a menu goes back to the previous menu. The path to BUTTON/DIAL
  SETTING is MENU, UP ×4, RIGHT. Its page 2 shows **SHOOT WITHOUT LENS: OFF** (the default), which confirms why
  the shutter did nothing. A run now turns it on and presses the shutter.
- Focus selector set to MF (physical key 47) with no lens: no change.

### M5 — first shutter attempts
- With a writable card, the shooting screen is clean: 545 shots, no card error. The firmware writes 2
  sectors to the card at mount.
- A battery warning appears ("use the NP‑W126S battery… boost mode restricted"). The firmware queries a
  battery gauge through `0x10a2cc0` (commands 0, 8, 0x10, 0x14, 0xce, probably I²C) and needs bit 15 of the
  reply to command 0. This is not modeled yet.
- Shutter half/full press injected into the key queue is accepted by the key handler (`0x10c606c`), but no
  capture starts. Discovery mode sees no hardware wait at all after the press. Leading hypothesis: no lens is
  attached ("F0") and "shoot without lens" (`UI_SETP_LENS_SW`, 1448) is off by default.
- Playback: the viewfinder turns black, but no playback screen appears. Cause: core 0 spins with
  interrupts masked in the memory‑controller driver (`0x108fdc8`, channel 1). It writes `0x200` to `+0x7080`
  and waits for `+0x7090` bit 0 to clear, but our register block declared that bit as always set. From the
  driver's tables: `0x100` → `+0x7090` low byte = 1 (boot), and `0x200` → bit 0 = 0 (before playback).
  These are now declared as two command laws instead of an idle bit.
- Power‑controller register 0 (0x8000, 0xC000, 0x8001) changes nothing on screen.
- The battery gauge (`0x10a2cc0`) is serial resource 0x11: same SPI controller as the power controller
  (`0xfff33000`), chip select GPIO 12.6. Its frames are an even command (bit 0 = write), two data bytes and a
  two's‑complement checksum. That differs from FujiMod's `ChecksumPowerSerial` (bit 7 = write), and the board
  accepts only one part per SPI controller. Both are to be extended later; the gauge seems to serve the
  VG‑XT3 grip batteries.

### SD card — write protect and second slot
- "CARD PROTECTED" came from the SDHCI host itself, not from a GPIO. `0x10878ac(slot)` reads Present State
  (`+0x24`) and treats **bit 19 = 1 as write protected**, the opposite of the SDHCI default, where 1 means the
  pin allows writes. The X70's write‑protect GPIO is active high too. New host option
  `write_protect_active_high` (default unchanged). Tests: SDHC init 9/9, board 11/11, XPP 119/119.
- GPIO 10.3 changes nothing: the card stays protected whether it is 0 or 1. `0x1085d94` muxes pins
  10.3/10.4 (slot 0) and 10.7/10.8 (slot 1) to the host.
- **Second slot**: card detect = GPIO 10.8, active low. This is physical key 14 in the catalog scan table,
  and GPIO 10.4 is key 13 "CARD". With 10.8 high, the slot‑1 card is not read. With **two cards**, both are
  mounted.
- M5 started: shutter half/full press (physical keys 2/3), playback, and a discovery variant.

### SD card — recognized and mounted
- With the correct interrupt lines, the driver runs the whole SD initialization, and the camera **mounts the
  FAT32 card**. It reads the MBR, the partition boot sector, both FATs and the root directory (1 MiB), with no
  wall in strict mode up to 2.4 G instructions.
- Card on slot 0 (`0xfffc4000`, GIC 230): the shooting screen shows **545 remaining shots**. Driver slot 0 is
  the camera's slot "1". The camera then reports "CARD PROTECTED", which points to a write‑protect input not
  yet wired.
- Card on slot 1 only: the camera still shows "CARD ERROR" for slot "1", the empty slot 0, whose card
  detect is driven low.
- SDHCI model: Host Control 2 bits 4–5 (Driver Strength Select, `0x30` after CMD6) are latched without
  effect. Tests: SDHC init 9/9, board 11/11.
- Four runs started to find the write‑protect and slot‑1 detect pins (GPIO 10.3, 10.7, 10.8) and to try two
  cards at once.

### SD card — interrupt lines found
- Call traces of 4 strict runs show that the driver **does send CMD0**, then waits up to 100 ms for the
  command‑complete interrupt, which never came: the emulated host was wired to the wrong GIC line.
- The firmware's interrupt‑source table (`0x13e6454`) gives the SD lines: **GIC 230** (slot 0 SDHCI),
  **237** (slot 1 SDHCI), 235 and 242 (UHS‑II banks).
- Units 3 and 4 are two instances of the card manager, one per slot. On insertion, the driver aborts the
  pending command wait (event flag `0x8000`). The media‑change handler then unmounts the slot and powers it
  off, and the remount fails.
- Four runs started with the corrected lines: card at boot, insertion, slot 0, and a discovery variant.

### SD card — tracing the card manager
- Two discovery runs with the card on `0xfffc8000` and a simulated insertion (IRQ 67 and 167): no polled
  register blocks the driver after the insertion, and no card command is sent. The two runs give identical
  results, so the IRQ line does not matter yet.
- The driver work after the insertion is a **slot power‑off** (`0x1081114`). It is called by the card manager
  in units 3/4: the "close" routine `0xd25500` powers the slot off when its user count reaches 0.
- `xp4_run`: new action `watch_calls`, which starts FujiMod's passive call recorder mid‑run and adds the
  calls (arguments, return values, callers) to the report. New tool `appels.py` builds the entry and return
  lists from every direct `bl` in the firmware units.
- Four traced runs started: insertion, card present at boot, no card, insertion with the card on slot 0.

### SD card — slots and insertion
- Slots are standard SDHCI hosts at `0xfffc4000` and `0xfffc8000`, each with a UHS‑II bank. Card detect is
  GPIO 10.4, active low.
- New XP4 `sd_hosts` declaration. SDHCI host options `capabilities` and `line_levels`, HC2 bit 12, clock
  word `+0x2E`. `xp4_run` action `gpio`.
- With a card at boot, or inserted later, the firmware configures the host but sends no command.

### M4 — menus (strict)
- The IMAGE QUALITY SETTING menu opens and the IMAGE SIZE sub‑menu is navigable. Up to 3.2 G instructions,
  no wall.

### M3 — shooting screen (strict)
- First‑start pages → shooting screen are bit‑identical in strict and discovery mode. The imaging extensions
  needed for live view are opt‑in.

### M2 — peripherals
- 66 register blocks with evidence. RAM is 1 GiB. Power registers 16 (power‑on reason) and 4 (battery).
  The memory controller has a command law. LCDC latches added.
- Device‑deadline cache: runs are 2.5× faster, with reports identical key by key. `dist` build: 5.9 M
  instructions/s.

### M1 — boot path
- XP4 board driven from the catalog. Loader start with 4 cores. Kernel, ThreadX and the application thread
  reached. Decompressed units match `fwtool.py`.

### M0 — build
- `fujimod-emulator` builds on macOS (cmake, pkgconf, Unicorn submodule, path without spaces, Swift
  compatibility library path).
