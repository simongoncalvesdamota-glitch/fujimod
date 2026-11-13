# Before publishing

Nothing in this folder goes to GitHub until the emulator is finished and the owner says go.

## Content

- [ ] No Fujifilm firmware bytes anywhere: no `FWUP0019.DAT`, no NOR image, no unit dumps, no
      disassembly listings longer than a few instructions, no byte arrays in JSON. Addresses, register
      values and SHA‑256 digests only.
- [ ] No personal paths (`/Users/…`), user names, e‑mail addresses or machine names in any file.
- [ ] Screenshots (`captures/`): they show firmware‑rendered UI. Decide which ones, if any, to attach.
- [ ] Every fact marked "to measure on hardware" is still marked that way, or has been measured.

## Upstream

- [ ] Re‑read the duplicates register (kept locally, not published) and drop anything already done upstream
      (`until: None`, sub‑word block access and control‑request blocks are in upstream PRs #183, #184, #185).
- [ ] Rebase the FujiMod changes on current upstream `main` (expected conflicts: `lcdc.rs`, `blocks.rs`,
      `sdhci/host.rs`, `sd.rs`, `emulator-session/src/tests.rs`).
- [ ] Re‑run the full test suites listed in [docs/fujimod-changes.md](docs/fujimod-changes.md) after the rebase.
- [ ] `.generated` catalog files regenerated with their generators, never edited by hand (`AGENTS.md`).
- [ ] One small pull request per topic (see [drafts/](drafts/)), each with its own tests.

## Posting

- [ ] The owner has reviewed each text and approved posting it under their account.
- [ ] Bring‑up issue first, then pull requests linking it.
