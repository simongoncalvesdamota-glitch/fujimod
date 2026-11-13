# Static analysis track — status and method (X-T3 5.11)

The static-analysis track locates stock firmware facts for the X-T3 (5.11) and
records them as catalog documents under `catalog/firmware/xt3/5.11/`. Progress is
tracked in `catalog/progress/xt3/5.11/progress.generated.json`
(`static` track: 15 of 22 facts located).

## Track status at a glance

Catalog `static` track: **8 of 11 stages pass**, **15 of 22 facts located**. The three
stages still `not-run` are each bounded by a hard limit — a missing builder or
device-only evidence — not by unfinished analysis. Counting the SoC boot ROM (a twelfth
static concern, outside the catalog track) the project checklist reads 8 of 12.

| Stage | Status | Where it stands |
|---|---|---|
| `research` | ✅ pass | firmware unpacked; 8/8 unit SHA-256 match the catalog |
| `catalog` | ✅ pass | catalog documents in place |
| `service-facts` | ✅ pass | |
| `bulk-source` | ✅ pass | |
| `flash-reader` | ✅ pass | XP4 packet-flash locator reproduced byte-for-byte against the image |
| `state-regions` | ✅ pass | |
| `controls` | ✅ pass | |
| `sd-facts` | ✅ pass | |
| `settings-layout` | ⛔ not-run | **Closed at the static ceiling.** Settings bank located and SHA-verified (flash `0xC0000`); region directory, 10-partition map and partition loaders all mapped. Only the builder-derived synthetic fields remain, and `fujimod-catalog-settings` is absent from the repository, so the official stage stays `not-run` (no hand bump). |
| `analysis` | ⛔ not-run | Six runtime facts (`ui_modes`, `menu_cursor`, `remote_menu`, `ff80_limits`, `usb_normal_ids`, `app80`) need live USB/screen dialogue with the camera (FF80). Static ceiling = 15/22. The same gaps appear on the X-T30 (same SoC). |
| `restart-facts` | ⛔ not-run | Power-off machinery located in the application unit (wake-arm, cut, halt, power-serial register map); the restart USB trigger ties to `app80` — the same device wall. |
| *(boot ROM)* | ⛔ wall | Mask ROM inside the SoC, absent from the firmware (`boot_rom = null`); every XP4 body reuses the X70 dump. Needs a device dump — a camera-track task, not static. |

**Bottom line.** Everything reachable by static analysis of the firmware image is done
and verified. The three open stages are not blocked on more reverse engineering: they
need either the absent FujiMod settings builder or evidence from a running camera (or
the emulator). The sections below record the method and the per-area findings.

## How facts are produced

Facts are **captured by a locator engine**, not written by hand:

- `crates/corpus` decodes ARM A32 / AArch64, builds cross-firmware function
  families, and exposes locator queries. It holds no camera maps; every map is a
  caller input.
- `crates/corpus/examples/capture_facts.rs` opens a prebuilt index (`Store`),
  reads `{firmware_id, processor, jedec_id, flash_dat_bias, update_path}` lines,
  and emits located facts. Locators wired today: `flash_update`, `boot_rom`,
  `boot_rom_audit`.
- `crates/corpus/src/locators/` additionally provides `flash`, `reset`,
  `update`, `image_offsets`.

So extending the track means **running (or wiring) a locator** against the index
and the firmware image, then shaping its output to the matching
`*.generated.schema.json`. The X70 (1.10) body is the complete reference for the
document shapes.

## Verified this pass

- The on-disk NOR image is 64 MiB and equals the catalogued flash window
  (`flash.generated.json`: address `0xF0000000`, size `0x4000000`).
- The flash `control` region digest in `flash.generated.json` **matches the real
  image** (SHA-256 of the first 32 KiB), so the located flash facts check out
  against the device image.
- The flash-reader code ranges resolve to genuine ARM routines (clean function
  prologues in the application unit), not placeholders.
- A function-prologue index of the application unit yields ~12.6k candidate
  function starts, usable as a locator seed.
- **The XP4 packet-flash locator was reproduced byte-for-byte against the image**
  (no index build required, since its layout is firmware-pinned): every routine
  prologue, the `0xf4000000` aperture compare, and the full reader contract
  (`verify_xp4_packet_reader`: word[0..8], word 56, the state address/value
  `movw/movt` pairs, and both branch targets) match. The committed
  `flash.generated.json` is therefore confirmed correct against the real
  firmware; the only difference from the current locator is cosmetic range
  naming.

## Remaining static stages and what unblocks them

| Stage | Document | Blocker / next step |
|---|---|---|
| Update file registered, unpacked, corpus exported | `firmware.json` | **Pass**: update package verified (size + SHA-256); outer layer (628-byte header + XOR 0xFF) reproduces the NOR image; all 8 units extracted (6 raw, 2 Method4) and every SHA-256 matches the catalog. The catalog listed unit-7 with unit-6's size; its real size (from the recovery recipe) is 9,494,528 bytes — corrected locally, to be proposed upstream |
| Function index built and facts located | same | Same index; export facts (see X70 `generated_facts`) |
| Flash reader located | `flash.generated.json` | **Pass**: locator reproduced byte-for-byte against the image |
| USB restart request located | *missing* `restart.generated.json` | **No corpus locator** (`reset` is reset-vector/boot-ROM audit); manual work, as on the X70 |
| Settings layout located | *missing* `settings.generated.json` | Built by the settings builder, not a locator; template is the X-T30 settings document; pivot is the config-data pointer |
| Boot ROM | `firmware.json` `boot_rom` (null) | The `boot_rom` locator declines XP4 (no established readable extent); `boot_rom_audit` only yields reset-vector diagnostics. Needs device-side evidence |

**Dependencies:** the update package is now staged and verified, and the decoded
corpus is exported. The remaining tooling gap is the corpus **index** (function
boundaries from a disassembler), which the function-index stage needs. The
restart and settings stages have no locator at all: restart is hand
reverse-engineered (as on the reference body) and settings is produced by the
settings builder, so both are fresh work for this body. Boot ROM needs
device-side evidence.

## Settings layout — bank located and verified

The settings store was reverse-engineered and pinned, combining static firmware
analysis with one emulator RAM snapshot (taken at 2,000,000,000 instructions) that
confirms the static map is not rewritten at runtime.

**Region directory (static).** The RAM region at `0xD20000` is the firmware's
"section table": 1,024 bytes loaded verbatim from flash `0x140000`
(SHA-256 `1e1b417d…`), confirmed byte-for-byte in the NOR image and byte-identical
in the live snapshot (zero bytes changed). Its header is `tableA = 0xD20040`,
`tableB = 0xD201EC`, `count = 0`; the accessor iterates to an `0xFF` terminator
rather than using the count.

**Partition map (table A).** Table A (36-byte entries: +0x04 RAM address, +0x08
loaded size, +0x14 flash address, +0x18 flash size) is the firmware's complete
flash↔RAM load map — ten partitions, each verified against the NOR image. It ties
every firmware unit to its flash offset (e.g. id 0x03 → flash `0x160000` → RAM
`0xD20400`; id 0x05 → flash `0x260000` → RAM `0xF21000`). The two 0x2ACC0-byte
modules at `0xD20400`/`0xE20400` are **code** modules, not the settings bank.

**Settings bank (pinned).** The settings bank is a 128-KiB slot at flash `0xC0000`,
read on demand into a RAM working copy at `0x03E68000` (the pointer the emulator
work had already captured). It uses the same field layout as the reference body.
The working copy equals the flash slot except for nine bytes written during init.

- **Source:** flash offset `0xC0000`, size `0x20000`, SHA-256
  `137ba8a997e8db51b076c70c975e42903ad212fb15f04d78ec4a6574be9f509f`.
- **Verified fields (flash defaults):** USB vendor ID `0x04CB` at +0x100; USB
  product ID `0x02DD` at +0x13E; body product name `"X-T3"` at +0x15C; settings
  bank signature `0xAA55` at +0x202.
- **Init-time writes (flash → live):** +0xD8 `0x00→0x80`; internal settings version
  +0x204 `0x0000→0x5802`; panel-timing counters near +0x406C.

**Honest limit.** A complete `settings.generated.json` also carries derived
"synthetic" fields (panel timing, proximity, accelerometer, focus knots) that the
settings builder computes from getter analysis, plus any wear-level bank metadata.
That builder is not in the repository, so those are not fabricated here. The output
of this pass is a hand-built proposal (source + partition map + identity fields +
SHA-256, derived/wear-level fields intentionally omitted); it does not flip the
catalog's official stage counter.

### Region loaders are partition loaders, not config banks

A short follow-up settled what the two "region readers" found during settings RE
actually do. Both stubs — `0x1120880` and `0x1120998` — are byte-identical except
for the region id they request (`mov r0,#1` vs `mov r0,#2`). Each calls the
id-accessor `0x11473E4`, which walks **table A** (`*0xD20000`; 0x24-byte entries,
first byte = id, `0xFF` terminator) and returns the matching descriptor. The stub
then reads `+0x14` (flash address) and `+0x18` (slot size), rounds the size up to
whole `0x20000` blocks, and copies them **raw** into `+0x04` (RAM destination) via
the block helper `0x111F5BC` → stock flash reader `0x107536C`. There is no
decompression: the loaded image equals the flash slot byte-for-byte.

So these are **resource/partition loaders**, not settings/config banks. The
id-accessor is called with ids 1, 2, 6, 7 and 8:

| id | loader | flash | slot | loaded | RAM | content | SHA-256 (content) |
|----|--------|-------|------|--------|-----|---------|-------------------|
| 1 | `0x1120880` | `0x1800000` | `0x580000` | `0x57E238` | `0x1E0000` | resource directory | `a7db8fd4…` |
| 2 | `0x1120998` | `0x1DC0000` | `0xC0000` | `0xA3EBD` | `0x7E0000` | **font pack** (identified by its header magic) | `748de575…` |
| 6 | `0x111F9CC` | `0x540000` | `0x480000` | `0x7D8000` | `0x1B4E000` | binary blob | `8b9976f0…` |
| 7 | `0x111FBC8` | `0x9C0000` | `0x920000` | `0x7D8000` | resolved at boot | binary blob | `cfa32ae6…` |
| 8 | `0x1120578` | `0x1620000` | `0x20000` | `0x0` | — | empty slot | `e3b0c442…` |

"Region 2" is therefore the **font-resource partition** (flash `0x1DC0000` → RAM
`0x7E0000`), not a second settings bank — confirmed by disassembly and by the flash
content matching the loaded image byte-for-byte. The settings bank stays the single
slot at flash `0xC0000`. This changes nothing in the catalog counter.

### The `analysis` gap is a locator-engine gap, not a device wall

A closer look at `catalog/generation.generated.json` (the machine output of the
locator engine, for every body) settles why the X-T3 `analysis` facts are still
`unavailable` — and corrects an earlier, too-broad "these need a camera" reading.

- **Every X-Processor 4/5 body is `facts: unavailable`** (X-T3, X-T30, X-T4, X100V,
  X-S10, X-Pro3, X-E4, X-T5). The seven bodies that are `available` are **all
  pre-XP4 SoCs** (X70, X30, X100T, X-E2, X-E2S, X-T10, X-T20). So no XP4 body has
  completed the static facts; the green marks on the progress dashboard are not
  backed by the locator output.
- **The engine is not ported to XP4.** For the X-T3 the `flash` locator reports
  *"xp4: flash reader contract is not implemented"* (although the flash reader was
  reproduced byte-for-byte), and `service` reports *"No service-workflow locator for
  X-Processor 4."* Only `bulk` and `controls` locators run on XP4.
- **The failing-locator set shrinks as locators mature**: X-T3 has 8 failing
  locators, the newer X-T5 only 2 (`app80`, near-complete, and `framebuffer`).
  Improving static code moves facts from fail to pass — which is impossible for a
  runtime-bound fact.

The firmware data is present and statically readable; the official engine simply
cannot verify it on XP4 yet. Two facts were hand-located this pass to demonstrate it:

| Fact | Status | Location |
|---|---|---|
| `ui_modes` | **located (static)** | HDMI-allowed predicate at `0xFF9FF0`; UI-mode state global `0x018C80E8` (mode byte `+0x120`, HDMI-off flag `+0x4C9`); HDMI allowed in modes {3,4,5,6} via the XP4 bit-mask idiom the pre-XP4 predicate doesn't recognize. |
| `usb_normal_ids` | **located (static)** | VID `0x04CB` / PID `0x02DD` in the settings bank (`+0x100` / `+0x13E`), anchored by the section table (`flash 0x140000 → 0xD20000`) that the locator reported missing. |
| `app80`, `ff80_limits`, `menu_cursor`, `remote_menu` | open (static) | need the XP4 service-workflow / menu locators, or deeper manual RE. |
| `framebuffer` | **runtime wall** | "Runtime scanout required"; still fails on X-T5. The one genuinely device-bound fact. |

**Honest ceiling.** With the two hand-located facts the static reach is at least
17/22, and higher with XP4 locator work — only `framebuffer` is truly runtime.
Flipping the official counter needs the XP4 locators themselves (in `crates/corpus`).

### Deep RE: the FF80 service and menu subsystems are located

Following the two hand-located facts, the harder locators (`app80`, `ff80_limits`,
`menu_cursor`) were traced from ASCII anchors via `MOVW`/`MOVT` cross-references.

- **FF80 service dispatch (`ff80_limits`, `app80`).** The string `"SERVICE: PCSS/1.0"`
  (RAM `0x022CFE18`) is referenced by function **`0x2143150`** — a service command
  dispatcher. It uses a jump table (`sub r4,#0x810; sub r3,#7; cmp #0x1F;
  ldrls pc,[pc,r3,lsl#2]`) with **command base `0x817`, 32 entries (`0x817..0x836`),
  16 implemented**. The full command→handler map is recorded alongside this track.
  **⚠️ Attribution corrected 2026-11-08 (see the update section at the end of this
  document): `0x2143150` is the *Wi-Fi* "PC-Shutter" (PCSS) service, not the USB FF80
  jig — `"PCSS/1.0"` is a network discovery-protocol header. The real USB service mode
  (04cb:ff80, vendor interface) is a separate subsystem, now located.**
- **Menu (`menu_cursor`, `remote_menu`).** The string `"UI:MenuTable is NULL"`
  (RAM `0x021FE9E4`) is referenced by function **`0x1D42F78`** — the menu command
  handler, which dispatches `0x80xx` command codes and updates menu-state globals at
  `0x0234xxxx`. The exact `menu_cursor_set_node` write remains to be pinned within it.

Confidence (rated conservatively): `ui_modes` and `usb_normal_ids` located/verified;
the `ff80_limits` dispatcher ~90%; `app80` ~60% (subsystem yes, writer/mailbox no);
`menu_cursor` ~55% (handler yes, set-node no). Nothing is fabricated. The point stands:
these facts live in the firmware and are reachable by static RE — only `framebuffer`
is genuinely runtime-bound. Completing them to an official pass needs the XP4 locators.

### End of the line: mailbox and menu context pinned

Pushing the two remaining locators as far as static evidence allows:

- **`app80` — service mailbox pinned.** The command-`0x817` handler (`0x2143274`)
  runs `memset(0x03172BC0, 0, 0x8F8)` then sets an active flag at `+0`, so the
  service mailbox is **`0x03172BC0`, size `0x8F8`**. The service app registration
  table is at `0x02323E8C` (21 entries of 12 bytes: context@+0, id@+8). The exact
  "app80 writer" is a FujiMod-internal locator name: thirteen functions write this
  mailbox and none stores a literal `0x80`, so the specific writer the locator wants
  cannot be uniquely verified without the locator source. Mailbox and dispatcher are
  located.
- **`menu_cursor` — menu context pinned.** The menu handler `0x1D42F78` updates a
  menu context global at **`0x018CC0E8`** (state byte at `+8`; fields at
  `+0x10/+0x14/+0x18/+0x48`) — a neighbour of the `ui_modes` state block
  (`0x018C80E8`) in the `0x018Cxxxx` UI-state region. The exact `menu_cursor_set_node`
  offset is not verified without the locator definition; the handler and context
  struct are located.

**Where static RE ends.** It reached the limit of what is verifiable without the XP4
locator sources: two facts proven (`ui_modes`, `usb_normal_ids`) and three subsystems
located with concrete addresses (service dispatcher `0x2143150` + mailbox `0x03172BC0`;
menu handler `0x1D42F78` + context `0x018CC0E8`). The exact offsets the locators
require are not fabricated. Only `framebuffer` is genuinely runtime-bound. Turning any
of this into an official pass still needs the XP4 locators themselves.

### restart-facts: power-off/restart machinery located, trigger verified

The `restart-facts` stage ("USB restart request located", `not-run`; the catalog's
`restart` table is null for xt3-511) was advanced with a cross-domain scout's framing
and then verified in the firmware.

**Mechanism.** The X-T3 does not reboot via a CPU reset register (`0xFFFxxxxx`); it
follows the camera pattern "arm a wake/restart state in the external power controller,
cut the rails, let the boot ROM reload." A shutdown/restart **task** `0x1147A4C`
(created by `0x11474F0`) drives it:

- **Reboot trigger (verified):** the task writes the power-serial control register
  `0x12` — value `0x800` (normal off) or **`0x8000` (wake/reboot-armed)** — via the
  power-serial write `0x10A20D4`. Disassembly: the halt path shows
  `r1=0x800; r0=0x12; bl 0x10A20D4` (off) versus, after `wake_arm 0x1147750` +
  `power_cut 0x1070D04`, `r1=0x8000; r0=0x12; bl 0x10A20D4` (reboot-armed).
- **Power cut:** `0x1070D04` (rail shutdown via the regulator API `0x10713D8`, a
  rails 0-7 jump table) — not a reset. **Halt:** `0x1147DCC` (`b .`).
- **Boot-source selector:** `0xFFF98008`, read by `0x1088634` (5-try retry loop) —
  the XP4 equivalent of the EXR boot-source selector; the XP4 map mirrors the EXR
  (`0xFFF88xxx` power, `0xFFF98xxx` boot-source).

**USB restart request (partial).** The orchestrator is an event-triggered task, and
the FF80 service handlers (`0x817..0x836`) do not call `power_cut` directly, so a
USB/service restart reaches it via a posted shutdown event. The firmware-update reboot
path (`PCSH_EV_UPDATE_FIRMWARE`, name thunk `0xF42EB0`) ends at the same power-serial
`0x12` path. The exact event poster — the precise "USB restart request" — is the one
remaining link. The stage stays `not-run` (no XP4 restart locator in `crates/corpus`);
this is a hand-built proposal, not an official pass.

## 2026-11-08 update — corrections and `identity_checks` solved

Deeper tracing (and a runtime call-trace confirmation from the emulator) corrected two
earlier attributions and solved a fact previously thought device-bound.

### Correction: `0x2143150` is the Wi-Fi "PC-Shutter" service, not the USB FF80 jig

The dispatcher `0x2143150` is owned by a thread created under the name **`wifi_pcsh`**, and
its commands `0x82F`/`0x830` parse an SSDP-style HTTP **network discovery** protocol
(`DISCOVERY … HTTP/1.1`, `HOST:`, `MX:`, `SERVICE: PCSS/1.0`, `NOTIFY`, `CAMERANAME:`,
`DSCPORT:`). So `PCSS/1.0` is the camera's **Wi-Fi tethered-shooting / discovery** service —
not the USB `04cb:ff80` jig. The earlier "FF80 USB dispatcher" reading was a conflation via
the shared `PCSS/1.0` string. The dispatcher **structure** (16 commands, jump table, mailbox
`0x96`, async event/callback model) stands; only the transport label changed.

The Wi-Fi remote command vocabulary is firmware-named (id→name registries): **`PCSH_EV_*`**
(17 ops, `0x8092..0x80A2`: capture, live view, get/set device property, storage/image info,
firmware update, backup settings) and **`WLAN_REMOTE_*`** (18 ops, `0x80A6..0x80B7`: live
view, zoom, S1-lock, capture, movie, exposure/aperture/shutter change). Both are dispatched
from the UI command register `*(0x018BF0E8)` (PCSH dispatcher `0x1B8F984`, WLAN `0x1B92040`).

### The real USB service mode (`04cb:ff80`) transport, located

The service USB device descriptor is fixed at **`0x22DC000`**: **VID `0x04CB`, PID `0xFF80`**,
interface class **`0xFF` (vendor-specific)** / sub `0x21` / proto `0xFF`, three endpoints —
**EP `0x01` OUT bulk** (commands), **EP `0x81` IN bulk** (responses), **EP `0x82` IN interrupt**
(events). This is the custom FTLUSB framed protocol, **not PTP**. Descriptor selection: a USB
mode byte at `0x2328000` (mode 3 = service) via provider `0x1D8DA74`; the normal-mode branch
reads the PID from the settings bank `+0x13E` — cross-confirming `usb_normal_ids`. The exact
FTLUSB command parser behind EP `0x01` OUT is a runtime-registered class callback (not a static
address), so it is the remaining layer for that subsystem.

### `identity_checks` — SOLVED (identity is ASCII in the settings bank)

The device identity is stored as **ASCII strings in the settings bank** (flash `0xC0000`), not
as binary dwords — which is why an earlier search for the binary values found nothing. Confirmed
against the NOR image, these match the public ffraw prior-art:

| Field | Value (ASCII) | Settings-bank offset | Flash address |
|---|---|---|---|
| IopCode | `FF159501` | `+0x4CC0` | `0xC4CC0` |
| IopCode (alt) | `FA159501` | `+0x4CD8` | `0xC4CD8` |
| ModelId | `FF180003` | `+0x7991` | `0xC7991` |

The PTP device property **`0xD184`** returns `"FF159501,FA159501"`. Its getter is
`0x1D01740 → 0x1CED088`, which reads the settings bank via the byte reader
`0x1CB3520 → 0x1149860 → 0x111EA34` — the **same** settings-bank reader `usb_normal_ids` uses
for the PID at `+0x13E` (`0x111EA34(offset)` → flash `0xC0000+offset`, verified: `0x111EA34(0x13E)`
→ PID `0x02DD`). The property is **mode-gated** by `0x1CFE9E4`, which reads the `ui_modes` mode
byte `0x018C8208` and only serves the identity in record mode (`UI_MODE_REC`) with an enable flag
set; otherwise it returns PTP error `0x80000040`. An emulator call-injection trace drove this
path correction. The values are per-device settings-bank content, but the **structure is exact
and firmware-grounded**: an XP4 locator can anchor on offsets `0x4CC0`/`0x4CD8`/`0x7991` and the
getter `0x1CED088`.

### `menu_cursor` — `set_node` is a `this`-method (static ceiling)

The `0x8029` "set-node" command (handler `0x1D4326C`) is a **refresh**: it syncs the UI mirror
`0x018CC0E8` from the menu object via the getters (snapshot `0xF21C5C`, node-id getter
`0xF224B8`, cursor getter `0xF22668`), reading `MENU_OBJ = *(0x017E2A04)`. It is **not** the
authoritative setter. Scans for a global-based setter (load `*(0x017E2A04)`, dereference, write
`+4`/`+8`) return zero, confirming `menu_cursor_set_node` writes `MENU_OBJ+4` (node id) / `+8`
(cursor) through a `this` passed in, on key navigation — the same shape the identity getter had
before a runtime trace resolved it. The data model is fully locator-ready; pinning the exact
`this`-method needs a runtime navigation trace.

### Net effect on the `analysis` ceiling

`identity_checks` joins `ui_modes` and `usb_normal_ids` as **statically located** (the structure;
values confirm public prior-art). `ff80_limits`/`app80` have the full Wi-Fi-PCSS dispatcher plus
the located USB service transport, with only the runtime-registered FTLUSB parser remaining;
`menu_cursor`/`remote_menu` have a complete data model with a `this`-method setter remaining;
`framebuffer` stays the one genuine runtime wall — independently corroborated by neighbour-brand
emulation (the Nikon D300 full-system emulator boots but produces no image either: its display is
driven over a bit-serial bus to the EXPEED coprocessor, with no CPU-visible framebuffer to scan
out), so this is an architecture-level runtime boundary, not a gap on our side. Nothing is
fabricated, and nothing here flips
the official counter — that still needs the XP4 locators in `crates/corpus`.

## 2026-11-08 — `menu_cursor_set_node` located (last analysis fact)

With the emulator differential dump giving the real list-cursor address, the `set_node` writer was
pinned statically. The settings menu's STATE STRUCT is at `0x17E2A04` and the list-line cursor is
its `+8` field (0x17E2A0C), the current node its `+0xC` (0x17E2A10). **`menu_cursor_set_node` =
function `0x1B628D0`** (runtime-confirmed to the instruction by the emulator): it searches for the next SELECTABLE row (skipping disabled rows via the
per-row flag array at `0x17E2A04+0x20+row`), then writes `[0x17E2A04+8] = cursor`, `[+0xC] =
node_table[cursor]`, `[+0x10] = aux`. These are the exact fields the runtime dump saw change on a
DOWN press, so the function is confirmed. Note the cursor is written through the GLOBAL struct
address (not a `this` pointer), so it is statically anchorable; an earlier "this-method" reading
had scanned the wrong field (the `+4` MENU_OBJ pointer rather than the struct's own cursor fields).
Also extracted this pass: the hardware `KEY_CODE_*` enumeration (190 codes; UP=0x1A, DOWN=0x1B,
LEFT=0x1C, RIGHT=0x1D, OK=0x16, FUNCTION=0x1E, …). With this, every `analysis` fact except
`framebuffer` (the genuine runtime wall) and `boot_rom` (device dump) is now located with concrete
addresses; the official counter still needs the XP4 locators in `crates/corpus`.

## 2026-11-08 — framebuffer located; full photo capture→image pipeline mapped

Three advances from the emulator-support work this session, each cross-validated against the
software job→GIC table (see below).

### Framebuffer located (last `analysis` runtime wall, now resolved)

The emulator's runtime dump located the LCD-controller framebuffer: **`0x474D000` (photo) /
`0x4BFD000` (video)**, LCDC instance 1, 720 × 480, BGRA8888. So the X-T3 *does* expose a
CPU-visible framebuffer (an LCDC scanout buffer in RAM), unlike the bit-serial-coprocessor displays
of some neighbour brands — the earlier "no CPU-visible framebuffer" characterization does not hold
for XP4. Remaining: the RAM pointer that selects between the two buffers (minor). With this, every
`analysis` fact except `boot_rom` (device dump) is located.

### Software job → GIC-line table

The authoritative map from logical software event ("job"/"source") to GIC interrupt line is a static
table of 132 × 16-byte records; an accessor returns `table + id*16`, and a registration routine wires
each handler to `record.field0` (= GIC line) through the GIC driver. The table is **runtime-confirmed**
on six sources from the emulator (0x56→GIC106, 0x52→107, 0x04→66, 0x22→59, 0x05→67, 0x71→144) and
further on job 0x44→GIC37 and job 0x0b→GIC135 (below). The imaging/capture cluster is jobs 75–87
(GIC 97–110). This table lets any observed IRQ be mapped to its software meaning.

### Photo still-capture pipeline (RAW → develop → encode)

End-to-end, all completion lines cross-checked against the job→GIC table:

| Stage | Engine | Completion |
|---|---|---|
| sensor → RAM (RAW) | sensor DMA | GIC 97 |
| RAW input + statistics | imaging input controller | GIC 106 |
| development (3 MOVDC channels) | output/develop units (reset reg, "go" reg, ack bit) | GIC 107 / 108 / 109 |
| JPEG encode | JPEG encoder engine | GIC 135 |
| (JPEG decode, read path) | JPEG decoder engine | GIC 136 |

The post-sensor stages are driven by a software **event state machine** with two syslog event
vocabularies: the capture events (groups by source) and a separate read-receive / post-processing
subsystem (resize / retouch / read-receive phases). Per-frame events (frame-sync, integration) must
be distinguished from one-shot capture events (RAW readback). An inter-core deferred-procedure-call
doorbell (a single-slot callback dispatched on GIC 144) schedules stages; it is a doorbell, not an
engine.

### Core-3 video stall — root cause corrected

The video record stall on core 3 is **not** a time-base artefact (a realistic time base and continuous
DMA position still fail deterministically at the 58th segment). The real cause: segment-complete
messages are sent to a message buffer whose receiver (the video pipeline's own event loop) is blocked
upstream on the unmodelled video codec; the buffer fills (~57 deep) and the 58th timed send returns a
house error code that is a remap of the µITRON `E_TMOUT`. Fix is to model the codec completion so the
loop keeps draining — same shape as the photo "missing completion IRQ" pattern.

### NOR flash controller identified (M6 / settings)

The engine at the "xp4-serial-flash" controller (completion GIC 37 / job 0x44) is the **NOR flash
write controller** (settings bank and NOR journal writes, firmware-update NOR writes) — shares the
driver cluster, lock and dirty-gate of the settings/NOR driver. (It was briefly mis-labelled an image
engine; corrected and removed from the photo path.) Relevant to the update-write (anti-brick) and
settings-flush milestones, not to photo.
