# X-T3 5.11 `analysis` facts: XP4 evidence for the eight failing locators

<!-- DRAFT — do not post. Waits for the emulator to be finished and the owner's go.
     Before posting: re-check the public count and the generation diagnostics on current main,
     re-run the local checker, fill every <…> placeholder. -->

## Request

Please review the source-backed candidates below for X-T3 firmware **5.11** (`xt3-511`) and, if they
satisfy the authoritative locator contract, incorporate them through the normal fact generator.
The current public `catalog/generation.generated.json` reports `facts: unavailable` for this firmware
with these locator errors:

```
firmware_id: xt3-511
ui_modes: ui_hdmi_allowed_in_current_mode: 1 candidates, 0 verified results (0xff9ff0: HDMI predicate lacks one checked playback/SETUP playback pair)
menu_cursor: menu_cursor_set_node: 0 candidates, 0 verified results
remote_menu: menu_cursor_set_node: 0 candidates, 0 verified results
ff80_limits: usb_service_debug_dispatch: 0 candidates, 0 verified results
usb_normal_ids: Factory flash/DAT bias lacks a unique checked section table
app80: App80 writer lacks the mailbox offset check
service_exit.jig_deinit_link: Defaults completion/action slot verified; full UI/SYS deinit routing is not yet located
framebuffer: Runtime scanout is required; static inspection cannot confirm a live framebuffer
```

The X-T3 public progress remains **15/22** <re-check on current main>; this report **does not claim
an accepted fact** and does not propose hand-editing generated catalog files.

## Original firmware provenance

- Official X-T3 5.11 updater `FWUP0019.DAT` SHA-256:
  `9f2c75fd8a16a1d71b9218f6dbd3144b1f11820bcecd3a9608c8cda9f25f95a3`
- Outer layer: 628-byte header, then the flash image XOR `0xFF`. Units unpacked at
  `628 + native_flash_offset`; units 5 and 6 are Method4-compressed. Decoded unit SHA-256s:

| Unit | RAM base | SHA-256 |
|---|---|---|
| 0 | `0x00040000` | `7357d741111e8367643795a5cf34666812fe09f91d3265e093dbff29bac5e71f` |
| 1 | `0x001E0000` | `a7db8fd4ccbba79a7b47efd86accd5ed8bf42a220e23632c4216ee6e1c3ef0a7` |
| 2 | `0x007E0000` | `748de5756e3114a56ce6baeb86fd321e3ff2a036dd882a654d53c96534f8f844` |
| 3 | `0x00D20400` | `4c92151d95163748e3bf1d21dd4e2c4b3cb66315d8ba6d5fda19ca5b2e8e2a23` |
| 4 | `0x00E20400` | `e70a3991014bc2e2a970f910a5d49dd43f1c5669636d93eadd82ec2a91745304` |
| 5 | `0x00F21000` | `7cd5f9f8cce57fc3d9100bf303ac01ae74735e5daa7392138ef73759a343d673` |
| 6 | `0x01B4E000` | `caa261813993aa407a9a7de5c6ebaa3f187c3926834fcb63a2b309681a50740a` |
| 7 | `0x033B1000` | `72178b9fff8a9931cc352d6400a578dad9483dedff317eb31b96b50f5cf9208a` |

  All eight match the catalog, except that unit 7 only matches with its real size **9,494,528** bytes
  (as in `recovery/recipes.generated.json`); `catalog/firmware/xt3/5.11/firmware.json` lists 8,224,768
  (unit 6's size).
- Addresses below are ARM32 (A32) addresses in that image. No firmware bytes, binary, disassembly
  listing or camera serial number is included. No camera was touched.

## How the values were checked

A local, deterministic checker re-finds each value **by pattern, with no result address hard-coded**,
then runs an independent check (listed per fact). Two runs give byte-identical output. It is an offline
verification, **not** the private upstream locator predicate. Happy to share it.

Reliability is stated conservatively: 100 = proven and replayable; 90 = one link is inference or
emulator-only; 50 = partial.

| Fact | Candidate | Reliability |
|---|---|---|
| `usb_normal_ids` | VID `0x04CB`, PID `0x02DD` | 100 |
| `ui_modes` | `play = 4`, `setup_play = 6` (rec 3, setup 5) | 90 |
| `menu_cursor` | `0x017E2A0C` (cursor struct `0x017E2A04`) | 90 |
| `framebuffer` | `0x0474D000`, 720×480 `bgra8888` (emulator) | 90 |
| `remote_menu` | cursor part only | 50 |
| `ff80_limits` | FF80 transport only | 50 |
| `service_exit.jig_deinit_link` | enum + default action slot only | 50 |
| `app80` | not found | — |

## `usb_normal_ids`

1. Descriptor provider **`0x01D8DA74`** loads a USB device-descriptor template and, for a USB mode
   argument other than 3, fills idVendor/idProduct (descriptor bytes 8–11) from settings-byte reader
   **`0x0111EA34`** with keys **`0x100, 0x101, 0x13E, 0x13F`**. For mode 3 it selects the fixed service
   descriptor `0x022DC000` (`04CB:FF80`) instead.
2. The settings bank's validity marker is the halfword at key **`0x202`**, read by `0x0111EA54` as
   `(key 0x203 << 8) | key 0x202` and compared with **`0xAA55`** by its callers in `0x0111F050`.
3. Exactly one 64 KiB-aligned block of the flash image carries that marker: **flash `0xC0000`**
   (128 KiB; RAM working copy `0x03E68000`). Keys `0x100/0x101` give `0x04CB`, keys `0x13E/0x13F` give
   `0x02DD`.

Independent checks: the service descriptor (an image constant) has the same VID, and its PID `0xFF80`
is the value stored right after the VID in the bank; the bank's model code appears in the `.DAT`
header's list of compatible bodies.

**What the locator needs to recognise:** on XP4 the "unique checked section table" is the settings bank
identified by its code-checked `0xAA55` marker at key `0x202`, then the four keys used by the
descriptor provider. This is the same chain as the X100VI report in #167, here in ARM32.

Limit: values come from the update image; a body's own bank could differ (not observed).

## `ui_modes`

- Predicate **`0x00FF9FF0`** (76 bytes; the candidate your locator already reports). It loads the UI
  state `0x018C80E8` and reads the mode byte at `+0x120`; HDMI is allowed for
  `(mode − 3) & 0xFD == 0` → {3, 5} or `(mode & 0xFD) == 4` → {4, 6}, then refused if
  `[state + 0x4C9] == 1`.
- Independent check: the firmware's own `UI_MODE_*` id→name registry (a separate compare chain) gives
  `REC = 3`, `PLAY = 4`, `SETUP_REC = 5`, `SETUP_PLAY = 6`, matching both groups exactly.
- **What the locator needs to recognise:** the XP4 bit-mask grouping (`sub #3; tst #0xFD` and
  `and #0xFD; cmp #4`) as the checked REC/SETUP and PLAY/SETUP_PLAY pairs, instead of two equality
  compares. (The X-T30 2.01 candidate `0xff5cf0` fails with the same message; not checked here.)
- Limit: that `+0x120` holds the `UI_MODE_*` value is inferred from the exact value match.

## `menu_cursor` (and the cursor part of `remote_menu`)

- `menu_cursor_set_node` = **`0x01B628D0`** (248 bytes). It is the only function that stores the
  cursor at `0x017E2A04 + 8` and then `node_table[cursor]` (node table at menu object `+0x14`,
  8-byte entries) at `0x017E2A04 + 0xC`. It returns early when the row count at `+0x18` is ≤ 1 and
  skips rows whose flag byte at `+0x20 + row` is > 1.
- Candidate: cursor struct **`0x017E2A04`** (`+0` menu object, `+8` cursor, `+0xC` node, `+0x18` row
  count, `+0x20` row flags), so **`menu_cursor = 0x017E2A0C`**, following the X70 record where
  `menu_cursor = cursor_address + 8`.
- Independent checks: other functions load the same base and read the cursor at `+8`. In emulation, a
  DOWN key press writes `0x017E2A00..0x017E2A20` only from `0x01B6299C` (cursor) and `0x01B629B0` (node).
- **What the locator needs to recognise:** on XP4 the struct is addressed absolutely (movw/movt) and
  the node is taken with an indexed load `[table, cursor, lsl #3]`.
- `remote_menu`: only the cursor part is located. `roots_address`, `root_count`, menu layout, roles
  and tabs are not. The menu command handler `0x01D42F78` (fixed codes `0x8050` open / `0x8029`
  refresh) is a lead for the roots table, not a result.

## `ff80_limits`

- Located: FF80 transport. Service descriptor `0x022DC000` = `04CB:FF80`, selected by `0x01D8DA74` in
  USB mode 3; one vendor interface `0xFF/0x21/0xFF` with endpoints `0x01` OUT bulk, `0x81` IN bulk
  (1024 bytes each) and `0x82` IN interrupt (8 bytes); configuration length consistent (57 bytes).
- **Rejected candidate:** jump-table dispatcher `0x02143150` (table `0x021431F4`, commands
  `0x817..0x836`). One handler references the `SERVICE: PCSS/1.0` discovery string and its task is
  named `wifi_pcsh`: it is the Wi-Fi PC-Shutter service, not `usb_service_debug_dispatch`.
- Not located: the FF80 request parser behind EP `0x01` and its bounds. It is registered at run time;
  on the X100VI it sits in a mode-3 FF80 callback table (#168). We did not find the X-T3 equivalent
  statically.
- Question: does `usb_service_debug_dispatch` start from the mode-3 callback table? If so, the
  descriptor provider's mode-3 branch above is our best entry point.

## `service_exit.jig_deinit_link`

- `UI_EXE_JIG_DEINIT = 0x10` (`UI_EXE_JIG_INIT = 0x0B`) from the `UI_EXE_*` id→name registry
  `0x00F5222C`. Default action slot = thunk **`0x01D17180`**, which only logs enum `0x10` through
  `0x00FFA060` (the UI_EXE log: 913 call sites, and it calls that registry). The thunks for `0x0E` and
  `0x0F` sit at the same `0x2C` stride; the address is computed, there is no pointer table.
- This matches what your locator already verifies (the action slot). The full UI/SYS deinit routing
  is still not located on our side either.

## `app80`

Not located. We find no static writer of an app80 opcode: the only mailbox-driven service dispatcher in
the image is the `wifi_pcsh` one above. The "write a byte at cfgdata `+0x80`" trigger described for
other bodies does not apply to this firmware (no access to bank `+0x80` anywhere). A run-time trace in
USB service mode would be needed.

## `framebuffer`

Run-time by definition. From the emulator (LCDC 1 register snapshots, shooting screen): source
**`0x0474D000`**, 720×480, `bgra8888`, stride 2880, instance 1, layer 1; movie mode uses `0x04BFD000`.
The display flip reads the buffer from `0x019597C4` (shooting context; movie context `0x019598FC`);
`address_mask 0x3FFFFFFF`. Statically, the display buffer table has a 720×480 record at `0x014FF2F8`
listing exactly these two buffers (contiguous, `0x4B0000` bytes each). Not confirmed on a real body.

## Precise maintainer help requested

1. Run or assess the upstream locators for `usb_normal_ids`, `ui_modes` and `menu_cursor` against
   the candidates above. If one fails, could you name the missing predicate?
2. For `ff80_limits`, `remote_menu` and `service_exit.jig_deinit_link`, could you share the expected
   proof shape, so that we bring a checked contribution rather than a guessed address?
3. If the evidence passes, publish the facts through the **authoritative** generator; we will not edit
   `*.generated.json` or assert a new count.

<details><summary>Range hashes</summary>

| Address | Size | Range | SHA-256 |
| --- | --- | --- | --- |
| `0x00ff9ff0` | `0x4c` | HDMI-allowed UI predicate | `d657782ababbf7baeb833f9eb5fe1afab0af0e01f38f706f4029ad921c47e00a` |
| `0x01b628d0` | `0xf8` | menu_cursor_set_node | `1500270229583497cc0cbeae06b663e3779bfd60915e3a3d40511616e78da1a7` |
| `0x01d8da74` | `0x128` | USB device-descriptor provider | `f041094b72907f988c00c9f94e4a2a9492adfc4769348fe0b3afc90f2d931e37` |
| `0x0111ea34` | `0x20` | settings byte reader | `12d767ae33a0999e24b0b891c284d1eca3ef4a9c283557ad5886488ae944155a` |
| `0x0111ea54` | `0x34` | settings marker reader (keys 0x203/0x202) | `e8d7769ba542eecb941a8fdd652ecd4f2576687bf45d0f72aaa00392ead9f4fb` |
| `0x01d17180` | `0x2c` | UI_EXE 0x10 default action thunk | `d45ed039f3bf46c99ad6fdd7e68f763b5d6682247cfd4f52a8c2cbb48dc35e80` |
| `0x02143150` | `0xa4` | PCSS dispatcher head (rejected) | `86a1aa91f2607d9b09295d80e2b43c6aedba51546f981eb0bc5fcb09efbb9896` |
| `0x022dc000` | `0x12` | FF80 service device descriptor | `c928045f6cdce351880c73879ddecc6bc41e7e515f0f87ae5d02212c641da715` |
| flash `0xc0000` | `0x20000` | settings bank (from the update image) | `137ba8a997e8db51b076c70c975e42903ad212fb15f04d78ec4a6574be9f509f` |

</details>
