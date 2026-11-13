# M5 — Photo: playback and still capture (in progress)

M5 has two halves. **Playback** (read a JPEG from the card and show it) works in the emulator.
**Still capture** (press the shutter and get a JPEG on the card) produces the RAW frame. The JPEG encoder also
works when it is driven on its own. But in a normal run the capture stops before the develop step. This page
records what is established, what was ruled out, and what is still open.

Reliability uses the project scale: **100 %** = proven and replayable, **90 %** = solid but partial,
**50 %** = hypothesis.

## Status at a glance

| Item | Status | Reliability |
|---|---|---|
| Playback: a JPEG on the card is decoded, resized and displayed with its Exif data | ✅ works | 100 % |
| JPEG decoder (`0xfffa1000`, GIC 136) | ✅ modelled | 100 % |
| Engines after the decoder (resize, `0xfa000000`, twin engines) | ✅ modelled (resize is an approximation) | 90 % |
| `/FFDB/FFXFER.DAT` written by the firmware on an empty card | ✅ observed | 100 % |
| Still capture: RAW frame from the emulated sensor | ✅ works | 100 % |
| JPEG encoder (`0xfffa0000`, GIC 135) driven on its own | ✅ works (discovery run) | 100 % for that path |
| Still capture, normal run: where it stops | ⛔ the sensor driver's read‑out operation never completes | location 100 % |
| Still capture, normal run: why | 🔍 a gate on two capture fields stops the "finish read‑out" command | 90 % from code and one run; to recheck on a clean run |
| Firmware colour profile (chroma matrix, luma bands, brightness gain) | 🧪 modelled, off by default (see below) | 50–90 % per stage |
| Still capture with a forced mode: develop chain | 🧪 runs up to the "retouch, before processing" request, no file written (corrected 2026‑11‑09, see below) | 100 % for that test run |

## Playback

### JPEG decoder

- The JPEG encoder and decoder are two separate blocks: encoder at `0xfffa0000`, decoder at `0xfffa1000`.
  Their register sets match the X‑T20, so the XP4 board reuses the X‑Processor Pro models
  (`jpeg_encoder` / `jpeg_decoder` board options).
- The decoder's interrupt is source `0x66` = **GIC 136** (level). An earlier model raised GIC 144, so every
  decode finished unnoticed, timed out and was retried four times before "READ ERROR". The handler reads the
  status at `+0x004`: bit 4 = done (it then reads `+0x014` and the output end `+0x108`), bit 17 = failure.
- The firmware's interrupt source table gives the job → GIC mapping used for all engines on this page.

### Engines after the decoder

Playback goes past three more waits after the decoder:

- `0xfa400000` (busy page): the firmware waits for `+0x518` bit 1 to clear.
- `0xfa000000`: an engine with a ready bit (`+0x00` bit 16) and a start bit (bit 0). It ends on **GIC 145**
  (level); its handler acknowledges through bit 3 of `0xfff4aa10`, then waits for the start bit to clear.
- `0xfff7e000` and `0xfff7f000`: twin engines missing from the catalog. They end on **GIC 132** and
  **GIC 133** (level); the handlers acknowledge by writing 1 to the status word.

These are covered by one generic model (`job_ack`): a register page whose job starts on a bit and is
acknowledged either through a word of another block or through a write‑one‑to‑clear status word.

### Resize engine

- `0xfff7e000` scales packed 4:2:2 pictures (Y0 Y1 Cb Cr). Registers: source and output sizes `+0x0c`/`+0x18`
  (w−1 | (h−1)<<16), ratios `+0x1c`/`+0x20` in Q12 (bit 16 halves first), source address and pitch
  `+0xc4`/`+0xd0`, output `+0xd8`/`+0xe4`.
- The model uses **nearest neighbour**; the engine's filter coefficients are not used. This is a declared
  approximation (90 %).
- In playback, one job scales the decoded 1920×1280 picture straight into the displayed video plane
  (720×480); the other jobs make the copies used for scrolling and a 1152×768 version.
- Checked: the displayed bars come in the same order as the source chart decoded on the host.

### Exif

- What the 5.11 Exif reader checks: APP1 "Exif" right after SOI, a TIFF header, IFD0 with the Exif pointer
  (`0x8769`), orientation, software and date.
- The tool `jpeg_exif.py` adds such a block to a test image (FUJIFILM / X‑T3, Exif 2.31). Playback then shows
  the photo screen with its Exif data: date, file 100‑0001, 1920×1280, 1/125 s, f/5.6, ±0, ISO 200.

### Card side effect

On an empty 8 GB FAT32 card, the firmware creates `/FFDB/FFXFER.DAT` (84,048 bytes, a FUJIFILM transfer
database dated by the camera clock) while starting up. Its directory and FAT entries are valid. The
emulator's card builder knows nothing of this folder, so the write comes from the firmware.

## Still capture

### What works

- **Shutter sound**: the electronic‑shutter capture arms audio DMA channel 0 as expected.
- **RAW frame**: the board had wired the sensor receiver without read‑out channels, so a requested still
  read‑out never ran. With the fix, the capture gets its frame: 6276 × 4212 RAW samples written to the
  buffer the firmware programmed.
- **Read‑out completion interrupt**: the develop engine's completion (GIC 107) is synthesised and its handler
  runs during the capture.

### JPEG encoder, on its own

In a discovery run (outside the normal capture path), a synthetic 4:2:2 test chart (720×480, eight bars)
is placed in RAM with a job descriptor using standard quantisation tables. The firmware's own encoder setup
and start routines are then called.

- The encoder's interrupt (job `0x0b`, **GIC 135**) is handled and reports success.
- The output is a 15,952‑byte JPEG (SOI … EOI) that decodes on the host to 720×480 with the eight bars
  correct. Two runs give bit‑identical output.
- So the existing encoder model works as it is. Caveat: a few isolated magenta pixels, probably the firmware
  writing into the RAM area chosen for the chart (to be checked with another area).

### Where a normal run stops (no forcing)

The capture task (called "imgdc" below, from its role) drives the sensor read‑out through a lower driver.

1. When the shutter is pressed, the driver starts a still read‑out operation and raises a "read‑out pending"
   flag. The capture task, once both halves of the capture are done, waits for that flag to clear before
   it moves on. (100 %, read from the code and seen in runs)
2. The read‑out's completion callback is called by the develop engine's interrupt (GIC 107) twice in the run.
   But it only completes the operation if the driver has first received a **"finish read‑out" command**
   (it moves the flag to a "ready to finish" state). That command is never sent. (100 %, runs)
3. How that command is meant to be sent (read from the code, then followed in a run):
   - the message that starts the read‑out also hooks a small callback on **GIC 100** (a periodic sensor
     timing interrupt; the hook list runs in the GIC 100 handler);
   - on the next GIC 100, the callback posts a message to the capture controller;
   - the handler of that message sends the "finish read‑out" command, **but only if a capture field is set**
     (and a second field selects whether the command is sent at once instead).
4. In the run: GIC 100 fires, the hook runs and the message arrives twice, but **the field is 0**, so the
   command is never sent. A second, backup path (in the capture task, on the first frame message) is also
   closed in that run. (100 % for that run)
5. Both fields are copied from a capture‑settings block at the start of the read‑out. **Open:** who fills
   that block, and whether the value 0 is right for this shot. (Open; under investigation)

**Caveat (bench artefact):** in the run used above, the bench left the half‑press button (S1) held down for
the firmware long before the shot. These fields may depend on the focus or shooting mode, so the result has
to be repeated on a run where the button is released properly. Until then, the cause above is rated 90 %.

### With a forced mode: how far the develop chain goes

As a test (a forcing, not a fix), the capture task's mode was set to a value that skips the wait in step 1.
With a few minimal register models added in the test plan, the capture then goes much further:

- the develop service starts and its engine completes (GIC 134);
- a processing engine (GIC 129) and its parameter banks are programmed; the parameter banks are plain
  registers with no wait;
- a resize page is programmed in a mode where source and output seem to be linked straight to other engines
  instead of RAM (50 %). The current resize model treated these as RAM addresses and stopped the run;
  without that model the capture continues;
- the capture reaches the resize step and then the **"retouch, before processing" request** (a post‑processing
  engine, named RETCH in the firmware), and live view resumes. The JPEG compression and "record media" steps
  that come after it are **not** reached in that run.

> **Correction (2026‑11‑09): the flow step names were off by 17.** The firmware keeps a table of named
> capture‑flow actions. An earlier reading started that name table 17 entries too late, so every action
> number was given the name of the action 17 places further on. With the right start, the two steps seen in
> the forced run read as "resize, before processing" (request) and "retouch, before processing" (request),
> not "JPEG" and "record media". Proof: the firmware's request handler has a case for exactly the even
> action numbers whose corrected names are requests (one request with no handler has no case either), the
> table ends with two "number of actions" entries that match the code's bound, and a run trace shows the
> second step launching the retouch process list. (100 %) The earlier text said "the capture reaches the
> JPEG step and then the record media reply".

**But no file is written**: the card shows the same few changed sectors as a run without capture. None of
these test models move any picture data. With the corrected step names, the reason is simpler: the run
never gets to the JPEG and "record media" steps. The likely blocker is the retouch engine, which has no
model yet: its driver waits for two "ready" status bits and then for its completion interrupt. (90 %, from
the code and one run; a model is planned, not yet tested.)

## Firmware colour profile (three options, all off by default)

The firmware programs three colour stages of the image processor for every film simulation. The emulator has a
model for each one, behind its own option. **All three stay off by default** (decision of 2026‑11‑10).

| Option | Register block | What it does (model) | Reliability | Main open point |
|---|---|---|---|---|
| `chroma_matrix` | `0xFFF87000` +0x238/+0x23c | a 2×2 gain matrix on centred Cb/Cr, after the YCbCr matrix | layout and values read: 100 %; it is a chroma gain: 90 % | which axis is Cb, the Q10 scale and where it sits in the chain: 50 % |
| `luma_bands` | `0xFFF87000` +0x258..+0x26c | a chroma gain that depends on Y: 4 knots, 5 bands, f(Y) = gain + offset·Y/256 (Q8), after the chroma matrix | law: 90 % | its place after the chroma matrix inherits that matrix's 50 % |
| `brightness_gain` | `0xFFF86000` (mode, weights, curve from +0x8) | a per‑pixel gain from a 40‑point curve indexed by a weighted luminance b, before the 3×3 colour matrix | table, weights and Q9 scale: 90 % | the 13 segment bounds and the scale of b: 50 % |

What the runs show (2026‑11‑10, emulator build with all three models):
- With every option off, runs are bit‑identical to the build without the models (100 %, replayed).
- Each option changes only the JPEG. Live‑view frames are identical in every run (100 %).
- `luma_bands` alone leaves Y bit‑identical and scales Cb/Cr by f(Y). On ETERNA, for example, colours below
  mid‑grey are untouched and chroma drops towards white, as the law predicts (100 % for the code).
- `brightness_gain` alone lifts neutral mid‑greys by 16 to 27 JPEG levels. Black and white do not move.
  A 32‑step grey ramp matches the curve to ±0.03 when b is the black‑subtracted 14‑bit value. This proves the
  code follows the model. It cannot prove that the camera uses the same axis and scale.

Why they stay off:
- Turning on any one option changes every reference JPEG of the test suite.
- The three stages form one chain. Turning one on without the others gives a picture no real camera makes.
- The plan is to turn them on together, as one "firmware colour" profile, once each one reaches at least 90 %,
  and to rebuild the reference JPEGs in one go.
- What would raise them to 90 %: a static reading of the segment bounds and of the chroma axis and scale, or
  a RAW + JPEG pair of the same scene from a real X‑T3.

## Wrong leads, ruled out

Each of these looked like the blocker and was closed with a runtime check or a correction.

| Lead | Why it looked right | Why it is wrong |
|---|---|---|
| **"Capture finished" codes `0x95`/`0x96`** | A saclac state machine waits for them, and their callbacks never ran | They are downstream of the real stop: nothing can finish while the read‑out operation is still pending. |
| **Missing GIC 105 completion** | A driver path waits for an engine that ends on GIC 105, never modelled | In a normal capture, that path is not used: the run shows the engine is never started. (An earlier run reading was wrong.) |
| **Self‑message purge loop** | The capture task has a loop of self‑messages that looked like the clean‑up of the pending operation | It is a wait loop for another condition, already true in our runs. It does not clear the pending operation. |
| **Factory "mode" byte** | The capture task's mode comes from a factory settings byte; another value skips the wait | Our value is the normal one for this body. Changing it emulates a different body and only bypasses the wait. |
| **Develop phase lock** | Read‑back‑end messages seemed missing because a per‑channel phase field held an unexpected value | For a normal still the phase is 0. Those messages are only posted for special capture kinds, so skipping them is by design. |
| **Busy request queue** | A pending entry made the camera report "not ready" | The entry is pushed at full press: it means "capture in progress", a consequence, not the cause. |
| **IIFP gate** | The develop worker sits behind a three‑condition gate | The gate is never reached in a normal capture. One of its conditions is even constant (always true). |
| **Callback with code `0x16`** | A saclac callback was expected to fire with that code | No site in the firmware calls it with that code. |
| **Lens flags** | Lens‑request flags select one capture branch | Forcing them before and after full press changes nothing; the body shoots without a lens by design. |
| **Lens‑driven "capture finished"** | A routine on the event path looked like a selector that picks the completion event from the lens state | It only sends a status notification to a client callback, and ran only before half press. |

Along the way, two documentation errors were corrected: the job descriptor first described as the
encoder's belongs to the **decoder** (`0xfffa1000`, GIC 136), and the encoder completion is GIC 135.

## Tools and models added for M5

- `job_ack` board model with a `resize` option (generic start‑bit / acknowledge engines).
- `jpeg_decoder` / `jpeg_encoder` board options (X‑Processor Pro models).
- `jpeg_exif.py`: adds an X‑T3‑style Exif block to a test image.
- `card_dump` bench action and a FAT32 reader (`carte_lister.py`) to inspect what the firmware writes.
- Counted run mode with differential code coverage, indirect‑call tracing and memory write watches, used
  for the runtime checks above.

## Sensor read‑out duration (static, firmware‑derived)

The firmware keeps two sensor‑mode tables (about 50 rows each). Each row gives the lines per frame, the line
length in clocks, and the native frame rate in millihertz. For every valid row, rate × lines × line length equals
72.000 MHz, so the sensor timing clock is 72 MHz. The code that turns a frame period into a line count does the
same calculation in reverse.

The emulated electronic‑shutter capture programs the row with 4300 lines × 418 clocks (40.06 frames/s):

| Quantity | Value | 54 MHz timer ticks |
|---|---|---|
| Full sensor frame (4300 lines) | 24.96 ms | 1 348 050 |
| Active lines only (4212) | 24.45 ms | 1 320 462 |

A slower full‑sensor family also exists (4280 lines × 790 clocks, 46.96 ms ≈ 1/21 s). Which family the real
camera uses for each shutter type or bit depth is not established yet.

## Next steps

1. Repeat the normal‑run check with the half‑press button released properly, then find what fills the two
   capture fields that gate the "finish read‑out" command.
2. Once the read‑out completes without forcing, model the develop engines that follow, with data movement,
   and fix the resize model for engine‑to‑engine links.
3. Model the retouch engine, then the steps after it (JPEG compression, then the media write), and check
   the JPEG written to the card.
