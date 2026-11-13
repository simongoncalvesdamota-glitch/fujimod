# GitHub drafts

Ready‑to‑post texts. One file per item. Post the issue first, then the pull requests, each linking the issue.
Fill in every `<…>` placeholder at publication time.

| File | Kind | Status |
|---|---|---|
| [00-issue-xt3-bringup.md](00-issue-xt3-bringup.md) | issue | draft; SD section to complete |
| [01-pr-power-serial.md](01-pr-power-serial.md) | PR | draft |
| [02-pr-lcdc-latches.md](02-pr-lcdc-latches.md) | PR | draft; rebase on the upstream `lcdc.rs` refactor |
| [03-pr-sdhci-reset-word.md](03-pr-sdhci-reset-word.md) | PR | draft |
| [04-pr-register-blocks.md](04-pr-register-blocks.md) | PR | draft; drop sub‑word access (#184) |
| [05-pr-catalog-xt3.md](05-pr-catalog-xt3.md) | PR | draft; XP4 `register_maps` for X‑T3 (71 register blocks, JPEG codecs, DRAM alias, serial ports) + list of models the schema lacks; candidate validated with `Soc::validate` |
| [06-issue-xt3-startup-card.md](06-issue-xt3-startup-card.md) | issue | draft; X‑T3 startup card reader wiped by the kernel (found in emulation) |
| [07-pr-xp4-settings-profile.md](07-pr-xp4-settings-profile.md) | PR | draft; builds on #184: XP4 profile applied (power registers 48–55 + NOR journal), more X‑T3 settings; movie values not yet checked |
| [../issue-xt3-511-locators.md](../issue-xt3-511-locators.md) | issue | draft; X‑T3 5.11 `analysis` facts evidence for the official locators (format of upstream #132/#167) |
| SD host options (`sd_hosts`, `capabilities`, `line_levels`, `write_protect_active_high`) | PR | card works (mount, 545 shots, two slots); cite Linux `SDHCI_QUIRK_INVERTED_WRITE_PROTECT` (F_SDH30) for the WP polarity; catalog needs two `card_slot`s |
| imaging opt‑ins (sensor, input buffer, MOVDC, statistics) | PR | to discuss first (the "do not pretend" policy) |
| `xp4_run` bench + loader start | PR | upstream may prefer it inside `emulator-boot` |

## Watch notes

Added by the hourly upstream watch (duplicates register kept locally). Newest last.

- 2026-11-07 — `05-pr-catalog-xt3.md`: upstream PR #184 also edits `catalog/soc/xp4/soc.json` (register
  blocks only; no RAM size, power‑controller inputs or card detect). Not a duplicate; merge `soc.json` after #184.
- 2026-11-07 — `04-pr-register-blocks.md`: upstream PR #186 (X‑T20 movie recording, stacked on #185) adds a
  PL330 `SourceWindow` to `crates/emulator-xpp/src/blocks.rs`. Not a duplicate of word‑aligned bases or
  self‑word commands; expect a rebase conflict.
- 2026-11-07 — SD host options (pending PR): upstream PR #184 (commit `11dcd254`) adds an XP4 `sd_host` latch bank over
  the card-controller windows at 0xfffc4000 (no card protocol) plus a `usb_condition` strap. Conflict: same `sd_host`
  name and addresses as our SDHCI-backed `sd_hosts`; rename and drop their SD windows when rebasing.
- 2026-11-07 — `02-pr-lcdc-latches.md`: upstream PR #184 (commits `70b322b8`, `ed698b85`) admits LCDC +0x28…+0x3c
  and the whole +0x40…+0x320 range (including +0x1c4), a superset of our latches. Duplicate: drop this draft if
  #184 merges, or reduce it to the strict-mode render evidence.
- 2026-11-07 — `05-pr-catalog-xt3.md`: upstream PR #184 (commit `a30bb09e`) sets the X‑T3 `dram_bytes` to 1 GiB,
  based on a RAM capture. Duplicate: remove the RAM bullet or cite #184 and add the MMU‑table evidence as support.
- 2026-11-07 — `02-pr-lcdc-latches.md` / `05-pr-catalog-xt3.md`: upstream PR #184 is now merged to main (LCDC range widened,
  X‑T3 `dram_bytes` = 1 GiB, XP4 `RegisterBank`). Duplicate: drop draft 02 (or keep only strict-mode render evidence) and
  remove the RAM bullet from draft 05; rebase both drafts on current main.
- 2026-11-07 — `03-pr-sdhci-reset-word.md`: upstream PR #191 (XP5 UHS‑II port) adds `pub mod uhs2;` to
  `crates/emulator-ip/src/sdhci.rs` and handles timeout (+0x2E) and software reset (+0x2F) per byte inside the port,
  not in the shared `Clocks`. Conflict: trivial rebase; be ready to justify putting the +0x2E word in `Clocks`.
- 2026-11-08 — `01-pr-power-serial.md`: upstream commit `018e68f` adds a declared exact command output and header
  admission to `power_serial.rs`. Conflict: rebase the new status/written words on it and check they do not overlap the
  declared command register.
- 2026-11-08 — `04-pr-register-blocks.md`: upstream PR #202 (open) adds `MovieOutput`/`scan_movie` to
  `crates/emulator-xpp/src/blocks.rs`; PR #186 is now merged. Conflict: rebase the word-aligned base and self-word
  command changes on current main.
- 2026-11-08 — `03-pr-sdhci-reset-word.md`: upstream PR #191 is now merged (`183c298`). Conflict: rebase on main.
- 2026-11-08 — SD host options (pending PR): upstream branch `worktree/ota` (39 commits, reconciled with main) splits `card_session` in `crates/emulator-session/src/tests.rs` into a power-cycle variant. Conflict: when rebasing, carry the three SD-option fields into `card_session_with_power`.
- 2026-11-08 — `07-pr-xp4-settings-profile.md`: upstream PR #216 (draft, unreviewed) renames `SettingsProfile.ram_address`
  to `loader_offset` (header delivered through the loader handoff block, offset 328) and declares the main journal,
  system journal and format stamp in NOR (blocks 1018, 992, 1016). Duplicate: rebase on #216 once merged and keep only
  what it lacks (header in power-controller registers 48–55, both sealed journal copies), and explain how the two
  header placements relate.
- 2026-11-08 — `01-pr-power-serial.md`: upstream PR #216 also widens `power_serial.rs` (inputs 21, 27; written 13, 14,
  17, 20, 23, 37). Duplicate: drop the registers it already covers and keep only register 16 (written) and inputs
  48..=55, after checking whether 5.11 reads 20, 21 and 23.
- 2026-11-08 — `04-pr-register-blocks.md`: upstream PR #202 is now merged (`666ea90`). Conflict: rebase the
  word-aligned base and self-word command changes on current main.
- 2026-11-08 — `07-pr-xp4-settings-profile.md`, `01-pr-power-serial.md`: upstream PR #216 is now out of draft (ready for review) and has merged main; still no maintainer review. Duplicate: no change to the plan, keep only the proven deltas for when #216 lands.
- 2026-11-09 — `07-pr-xp4-settings-profile.md`, `01-pr-power-serial.md`: upstream PR #216 is now merged (shorter than announced: inputs 21 and 27; written 13, 14, 18, 20, 37; `loader_offset` = 328). Duplicate: rebase on main and keep only the proven deltas (register 16 written, inputs 13 and 48–55, header in 48–55, both sealed copies).
- 2026-11-09 — `01-pr-power-serial.md`: upstream branch `heblushabus-xt3-rec-boot` (commit `1bf50ded`) sets REC power values 0, 3, 4, 5 with register 4 = 695. Conflict: our value is 900 against a firmware threshold of 752; settle the scale before submitting.
- 2026-11-09 — `02-pr-lcdc-latches.md`: upstream branch `heblushabus-xt3-rec-boot` (commit `22e7e581`) adds a layer count to the LCDC frame status and reports strict live view on the rear LCD. Conflict: rebase our `+0x28`/`+0x30`/`+0x1c4` latches on it.
- 2026-11-09 — `05-pr-catalog-xt3.md`: upstream branch `heblushabus-xt3-rec-boot` adds DRAM controller words, control request blocks at 0xff70a000–0xff70f000 and unit reset ready bits at 0xfff50000 / 0xfff74000 to `soc.json`. Conflict: drop any of our blocks it covers once it lands.
- 2026-11-09 — imaging opt-in (pending topic): upstream branch `heblushabus-xt3-rec-boot` retains unmodeled words of the sensor command and MOVDC windows and reads the whole statistics bank back. Duplicate: compare with our `latch_configuration()` and statistics window before writing this PR.
- 2026-11-09 — SD host options (pending PR): upstream commits `ac9b592` and `0ab8c3d` add a one-shot delayed CMD8 timeout fault (`RejectCmd8OnceAfter`) to `sdhci/host.rs` and its initialization test. Conflict: rebase the `capabilities` / `line_levels` / `write_protect_active_high` options onto them (neighbouring `SdHost` fields and snapshot).
- 2026-11-09 — `05-pr-catalog-xt3.md`: upstream branch `heblushabus-xt3-rec-boot` (commit `e3c596f3`) declares the X-T3 DesignWare USB device controller at 0xffb00000 (interrupt 246) in `soc.json` and a `usb_cable` hardware input. Conflict: do not add our own USB entries to the catalog PR; rebase onto it once it lands.
- 2026-11-09 — `08-pr-xp4-counted-execution.md`: upstream commits `d581dc2`, `1fdbc17` and `a9d6959` move the vendored Unicorn submodule to `73f5a7f` (translation lookup cache added, then removed). Conflict: rebase the `JIT_CALLBACK_GUARD` change onto the new submodule head and re-measure the speed-ups.
- 2026-11-09 — `08-pr-xp4-counted-execution.md`: upstream PR #224 adds a vector-context preflight (`preflight_with_vector_context`) inside the XP4 board's per-instruction hook. Conflict: keep that preflight on our fast path when rebasing.
- 2026-11-09 — SD host options (pending PR): upstream branch `heblushabus-xt3-rec-boot` (commits `7ac67268`…`f14c89af`) adds an XP4 SD host (`emulator-xp4-io/src/sd.rs`) and declares both slots (0xfffc4000 / interrupt 230, 0xfffc8000 / interrupt 237) with capabilities, capabilities 2, version and present-state values read on a real X-T3. Duplicate: drop our own slot wiring if it lands and reuse its measured values for the `capabilities` / line-level options.
- 2026-11-09 — SD host options (pending PR): upstream commits `fce41df` and `07bd7b2` latch normal and error status only when enabled in +0x34 / +0x36, for every host. Conflict: rebase our options onto it and check that the XP4 empty-slot timeout still raises an interrupt.
- 2026-11-09 — SD host options (pending PR): upstream commit `7f26af1` adds `mod ram_option;` to `crates/emulator-session/src/tests.rs`, next to our new `card_session` fields (`capabilities`, `line_levels`, `write_protect_active_high`). Conflict (trivial, disjoint hunks): keep both when rebasing; no change to the draft text.
- 2026-11-09 — SD host options (pending PR): upstream fork branch `BlancRyuuzaki-main` (commit `62b7b880`) adds a read-only `capabilities_1` input at +0x44 to `sdhci/host.rs`, checked on restore, plus word-aligned host bases and Host Control 2 bits 4-5 and 12. Duplicate (partial): if it lands, align our `capabilities` option with its name/shape (or propose the two-word +0x40/+0x44 form) and keep only `line_levels` / `write_protect_active_high` as new.
- 2026-11-10 — SD host options (pending PR): upstream commits `a2c3710` and `388925d` let the SD card model accept CMD7 deselect and CMD3 in Stand-by, and add a fault that refuses it. Conflict: rebase our `host.rs` options onto them (different hunks) and rerun the SD session tests.
- 2026-11-10 — `05-pr-catalog-xt3.md`: upstream branch `heblushabus-xt3-rec-boot` (commits `ed5432cb`, `d8815b43`) strips firmware addresses from the `basis` texts in `catalog/soc/xp4/soc.json` and sets the X-T3 `analog_levels` to 4096 (12-bit). Reuse: write our `basis` texts without firmware addresses, and align the analog scale once checked against our samples.
- 2026-11-10 — SD host options (pending PR): upstream PR #253 rewrites `sdhci/host.rs` (including the +0x2E clock-word store) and the `emulator-session` tests to pass `clippy -D warnings`. Conflict: rebase our options onto it if it lands and keep the new code clippy-clean (let-chains).
- 2026-11-10 — imaging opt-in (pending topic): upstream PR #253 rewrites `emulator-sensor-input` (`sensor.rs`, `slvs.rs`, `statistics.rs`, `input_buffer.rs`) for clippy. Conflict: style-only hunks; rebase and rerun the sensor tests.
- 2026-11-10 — `08-pr-xp4-counted-execution.md`: upstream commit `9a86ed7` moves the vendored Unicorn submodule to `b04668d` and adds per-cause `translation_invalidations_*` counters. Conflict: rebase the `JIT_CALLBACK_GUARD` change onto `b04668d`; use the new counters when reporting the XP4 speed-up.
- 2026-11-10 — `05-pr-catalog-xt3.md`: upstream branch `heblushabus-xt3-rec-boot` (commit `7667e0fe`) declares the XP4 indexed USB PHY port in `soc.json` and its X-T3 reset state as a hardware input. Conflict: do not declare the USB PHY in our catalog PR; rebase onto it.
- 2026-11-10 — `08-pr-xp4-counted-execution.md` and SD host options (pending PR): upstream PR #253 is now merged (clippy `-D warnings` across the workspace, rewriting `aprofile.rs` and `sdhci/host.rs`). Conflict: rebase both onto current main and keep the new hunks clippy-clean before opening anything.
- 2026-11-11 — `03-pr-sdhci-reset-word.md`: upstream PR #264 accepts Software Reset bits 1 and 2 (CMD / DAT line) as a byte write at +0x2F, resets only those lines, and keeps the +0x2E halfword path. Conflict: rebase onto it if it lands and check that our `sd_clocks` window covering +0x2c…+0x2f does not shadow the host's +0x2F handling; cite it in the draft.
- 2026-11-11 — SD host options (pending PR): upstream PR #264 adds a polled host mode (status enabled at +0x34 with no signal enable at +0x38), CMD6 status read through the data port, and per-block PIO sizes in `sdhci/host.rs`. Conflict: rebase our `capabilities` / `line_levels` options onto it and rerun the SD session tests.
- 2026-11-11 — `08-pr-xp4-counted-execution.md`: upstream PR #264 pins the vendored Unicorn to justjake/unicorn#4 (MMU-off alignment check, per-target `uc_invalidate_exit_tb` name). Conflict: rebase `JIT_CALLBACK_GUARD` onto that pin and confirm the alignment check stays active on our fast path.
- 2026-11-11 — SD host options (pending PR): upstream PR #268 adds an opt-in SD card timing model (`sdhci/latency.rs`, a `latency` field in the host `Options`, an XPP SD wrapper option, one-line updates to the initialization and session tests). Conflict: rebase our `capabilities` / `line_levels` / `write_protect_active_high` fields next to `latency` and carry them into the same test initializers.
- 2026-11-11 — SD host options (pending PR): upstream fork branch `BlancRyuuzaki-main` now adds a declared `retained_registers` list (vendor offsets 0x100..0x200 read back their last write) and a card "initialized by an earlier boot stage" mode (ACMD41 not ready until a declared supply-off interval) to `sdhci/host.rs`. Conflict: if it becomes a PR, rebase our `capabilities` / `line_levels` options onto its `Options` and keep both checkpoint paths.
- 2026-11-11 — `03-pr-sdhci-reset-word.md`: upstream PR #264 is now merged into main (commit `2bffa58`), including its +0x2F byte reset of CMD/DAT lines and the vendored Unicorn bump. Conflict: rebase the draft and `08-pr-xp4-counted-execution.md` on main before publishing, and drop any part of the reset handling that main now covers.
- 2026-11-11 — SD host options (pending PR): upstream fork branch `BlancRyuuzaki-main` adds commit `e85790f3` (card programming busy after writes). Conflict: if it lands before our SD host options, rebase onto it and keep busy modelling off by default for the XP4 photo-save path.
- 2026-11-12 — SD host options (pending PR): upstream PR #286 (open, uncompiled draft) ports the remaining fast-read host changes (CMD6 status over the data port, polled host signals when signal-enable is zero, CMD/DAT line resets at +0x2F, PIO block size). Conflict: rebase `sdhci/host.rs` onto it if it lands; recheck the `rom = true` host path and drop any reset handling it now covers from `03-pr-sdhci-reset-word.md`.
- 2026-11-12 — `05-pr-catalog-xt3.md`: upstream fork branches `byxk/xt3-emu-capture` and `byxk/xt3-emu-still-capture` declare in `soc.json` the XP4 JPEG encoder (0xfffa0000, IRQ 135), RAW read-out DMA IRQs 97/98, the still engines and DSP host, and extend the xt3-511 run's DRAM space to 0xf0000000. Duplicate: do not declare these blocks in our catalog PR; rebase onto them if they reach a PR.
- 2026-11-12 — imaging opt-in (pending topic): upstream fork branches `byxk/xt3-emu-*` change `emulator-sensor-input` (sensor command words accepted while a command is pending, RAW DMA channel windows) and `lcdc.rs`/`movdc.rs` (via the merged REC boot chain). Conflict: rebase our sensor/MOVDC hunks onto them and drop any overlap.
- 2026-11-13 — `01-pr-power-serial.md`: upstream PR #294 (merged, commit `c893b52`) lets the power controller accept configuration registers 48..55 as declared initial values. Duplicate: drop the 48..55 part from the draft and rebase; upstream branch `heblushabus-xt3-rec-boot` commit `7d444d12` also declares input 16 = 4 (cold-boot reason), compare it with our value before publishing.
- 2026-11-13 — `02-pr-lcdc-latches.md`: upstream PR #294 (merged, commits `90c5d90` / `abe264d`) reads the XP4 LCD picture plane as alpha, blue, green, red bytes (`Abgr8888Straight`, checked on a real rear screen) and publishes the layer count in the frame state. Conflict: rebase `lcdc.rs` and `framebuffer-xp4` onto it and switch our BGRA naming and tests to ABGR.
- 2026-11-13 — `05-pr-catalog-xt3.md`: upstream PR #294 is merged, so the JPEG encoder, RAW DMA IRQs, still engines, viewfinder blocks, face detection and the 0xf0000000 DRAM extent are now in main `soc.json` / xt3-511 run. Duplicate: remove those entries from the draft and keep only what main still lacks.
- 2026-11-13 — imaging opt-in and SD host options (pending topics): upstream PR #294 (merged) changes `sensor.rs` / `slvs.rs` / `statistics.rs` / `movdc.rs` and adds SD card media on XP4 boards; open PR #291 adds a pluggable SDHCI card slot (`CardDevice`) and RAM-backed statistics. Conflict: rebase both topics on main, then onto #291 if it lands.
