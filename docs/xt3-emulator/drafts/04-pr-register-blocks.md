# PR draft — xpp: register blocks with word‑aligned bases and self‑word commands

**Title:** emulator-xpp: allow word‑aligned register blocks and commands that acknowledge on their own word

## Summary
- A block base only has to be word‑aligned, as long as the block fits in one page. A block can then start
  right after another device window in the same page (X‑T3: `0xfff4a520`, after the DMA mux).
- A command law may name its own word as its status word. This gives a self‑clearing "busy" bit: the job
  completes at once.

Sub‑word (8/16‑bit) access is left out: #184 already does it.

## Tests
XPP 119/119, XP4 board 11/11.
