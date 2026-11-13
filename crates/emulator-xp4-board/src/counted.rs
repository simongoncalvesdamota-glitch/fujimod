//! Counted ARMv7-A execution: the same execution law as the per-instruction
//! hook, run in native translated blocks.
//!
//! The per-instruction hook does, before every instruction: due() (device
//! deadlines, interval timers, wake-ups), the cache-coherence redirect, IRQ
//! entry, then counts the instruction (guest time), observes labels/returns
//! and performs the board's own instructions (exclusives, identity reads,
//! WFI/WFE/SEV, PMU). Counted execution keeps every one of those steps at the
//! same instruction boundary:
//! - a slice is cut into internal runs that end exactly where due() has work
//!   (`room`), so deadlines fire before the same instruction;
//! - device accesses resynchronise guest time from the exact entry count and
//!   end the run after their instruction whenever due(), the redirect or an
//!   IRQ would act before the next one;
//! - an asserted IRQ stops the processor before the next block once it is
//!   unmasked; the board enters it at that boundary, as the hook does;
//! - board instructions exit before entry and run through `special`;
//! - labels and call returns are single-address observers.
//! Internal run boundaries are invisible to the round-robin scheduler, which
//! still sees each slice end where the per-instruction hook ends it.
//!
//! - memory watches observe data accesses through hooks on the watched
//!   ranges and instruction fetches through observers on the same ranges
//!   (XP4 code runs at its physical address); a boundary records the fetch
//!   the hook would record before an IRQ, redirect or board instruction;
//! - recorder (function watch) entries and returns are single-address
//!   observers; an entry observed twice (restart, overlap) is reported once.
//! Not kept: the per-core trace of the last 32 entered instructions (only
//! the per-instruction hook records it). The driver guard and PC observers
//! are refused, and discovery, the low boot alias and the PMU profile fall
//! back to the per-instruction hook; run those without `FUJIMOD_XP4_COUNTED`.
use super::*;
use fujimod_emulator_cpu::Interrupt;

/// Indirect-branch edges seen at block entries: the previous block of the
/// same core ended with a computed branch (not a return) and this block is
/// where it went. Board-performed instructions, IRQ and abort entries reset
/// the predecessor, so their discontinuities are never edges.
#[derive(Default)]
pub(super) struct Edges {
    last: Vec<Option<(u32, u32)>>,
    kinds: std::collections::HashMap<u32, Option<&'static str>>,
    pub(super) seen: std::collections::BTreeMap<(u32, u32), (u64, &'static str)>,
}

/// ARM-state computed branches, excluding function and exception returns.
fn indirect_kind(word: u32) -> Option<&'static str> {
    if word >> 28 == 15 {
        return None;
    }
    let rn = (word >> 16) & 15;
    let rm = word & 15;
    match word & 0x0fff_fff0 {
        0x012f_ff30 => return Some("blx"),
        0x012f_ff10 if rm != 14 => return Some("bx"),
        0x012f_ff10 => return None,
        _ => {}
    }
    // Data processing with Rd = PC and S clear (S set is an exception return).
    if word & 0x0c00_f000 == 0x0000_f000 && word & (1 << 20) == 0 && word & 0x0f90_0000 != 0x0100_0000 {
        let opcode = (word >> 21) & 15;
        if opcode == 13 && word & (1 << 25) == 0 && rm == 14 && word & 0xff0 == 0 {
            return None; // mov pc, lr
        }
        return Some(if opcode == 4 { "add-pc" } else { "dp-pc" });
    }
    // LDR PC (word, load), not a pop.
    if word & 0x0c50_f000 == 0x0410_f000 {
        return (rn != 13).then_some("ldr-pc");
    }
    // LDM with PC in the list, not a pop; LDM^ with PC is an exception return.
    if word & 0x0e10_8000 == 0x0810_8000 {
        return (rn != 13 && word & (1 << 22) == 0).then_some("ldm-pc");
    }
    None
}

pub(super) fn requested() -> bool {
    std::env::var_os("FUJIMOD_XP4_COUNTED").is_some_and(|v| v != "0")
}

impl Cluster {
    /// Accounts `n` entered instructions of `core` exactly as `n` single
    /// steps of the per-instruction hook do.
    pub(super) fn advance(&self, core: usize, n: u64) -> Result<(), String> {
        if n == 0 {
            return Ok(());
        }
        let state = unsafe { &mut *self.state.get() };
        state.instructions += n;
        let (ticks, phase) = if self.per_core_time.get() {
            // Phase in 1/(12 * instructions) ticks: 12 / n is whole for n <= 4 cores.
            let (instructions, per) = self.instruction_rate.get().parts();
            let runnable = 1 + state
                .waiting
                .iter()
                .enumerate()
                .filter(|&(c, &w)| c != core && w == 0)
                .count() as u64;
            let unit = u128::from(12 * instructions);
            let total = u128::from(state.tick_phase) + u128::from(n) * u128::from(per * (12 / runnable));
            (total / unit, (total % unit) as u64)
        } else {
            self.instruction_rate.get().advance_phase(state.tick_phase, n)?
        };
        state.ticks = state
            .ticks
            .checked_add(u64::try_from(ticks).map_err(|_| "A7 guest time overflow")?)
            .ok_or("A7 guest time overflow")?;
        state.tick_phase = phase;
        state.per_core[core] += n;
        state.waiting[core] = 0;
        Ok(())
    }
    /// Instructions `core` may still enter before due() has work: the first
    /// one whose accounting reaches the next device deadline or interval
    /// expiry ends the run, so due() runs before the following instruction.
    pub(super) fn room(&self, core: usize) -> Result<u64, String> {
        let state = unsafe { &mut *self.state.get() };
        let now = state.ticks;
        let device = state.devices.as_mut().and_then(|d| d.settled_deadline(now));
        let timer = state.timer.as_ref().and_then(|t| t.next_event_tick(now));
        let Some(at) = device.into_iter().chain(timer).min() else {
            return Ok(u64::MAX);
        };
        if at <= now {
            return Ok(0);
        }
        if self.per_core_time.get() {
            let (instructions, per) = self.instruction_rate.get().parts();
            let runnable = 1 + state
                .waiting
                .iter()
                .enumerate()
                .filter(|&(c, &w)| c != core && w == 0)
                .count() as u64;
            let unit = u128::from(12 * instructions);
            let step = u128::from(per * (12 / runnable));
            let need = u128::from(at - now) * unit - u128::from(state.tick_phase);
            Ok(need.div_ceil(step).min(u128::from(u64::MAX)) as u64)
        } else {
            self.instruction_rate.get().budget(now, state.tick_phase, at)
        }
    }
    /// Brings guest time up to the running processor's exact entry count
    /// (the current instruction included).
    pub(super) fn sync_counted(&self, core: usize) -> Result<(), String> {
        let progress = self.cpus[core]
            .instruction_progress()
            .ok_or("counted XP4 run without an entry count")?;
        self.sync_to(core, progress)
    }
    /// Accounts the running processor's entries up to `progress`.
    fn sync_to(&self, core: usize, progress: u64) -> Result<(), String> {
        let state = unsafe { &mut *self.state.get() };
        let delta = progress
            .checked_sub(state.accounted)
            .ok_or("counted XP4 entry count moved backward")?;
        state.accounted = progress;
        self.advance(core, delta)
    }
    /// Installs a single-address counted observer at each address `core`
    /// does not observe yet (the processor must be stopped).
    pub(super) fn observe_addresses(&self, core: usize, addresses: &[u32]) -> Result<(), String> {
        let data = (&*self.hooks[core] as *const Hook).cast_mut().cast();
        for &address in addresses {
            let state = unsafe { &mut *self.state.get() };
            if !state.observed[core].insert(address) {
                continue;
            }
            mark_observed(&mut state.observe_filter, u64::from(address));
            unsafe {
                self.cpus[core].install_hook(fujimod_emulator_cpu::Hook::Instruction {
                    callback: counted_observer,
                    data,
                    range: HookRange::Inclusive {
                        begin: u64::from(address),
                        end: u64::from(address),
                    },
                })
            }?;
        }
        Ok(())
    }
    /// After a device access inside a counted run: end the run after this
    /// instruction whenever the per-instruction hook would act before the
    /// next one, otherwise shorten it to the (possibly moved) next deadline.
    pub(super) fn after_access(&self, core: usize) -> Result<(), String> {
        let state = unsafe { &*self.state.get() };
        let pending = state.gic.next_irq(core).is_some();
        let wakes = (0..self.cpus.len())
            .any(|c| c != core && state.waiting[c] != 0 && state.gic.next_irq(c).is_some());
        let stop = state.failure.is_some()
            || state.cache_dirty
            || state.remap_pending
            || wakes
            || (pending && self.reg(core, arm::CPSR)? & 0x80 == 0);
        let cpu = &self.cpus[core];
        cpu.set_interrupt(Interrupt::Irq, pending)?;
        let progress = state.accounted;
        let room = if stop { 0 } else { self.room(core)? };
        cpu.tighten_instruction_limit(progress.saturating_add(room))
    }
    /// What the per-instruction hook does before entering the next
    /// instruction. `true` ends the slice (redirect or IRQ entry).
    fn boundary(&self, core: usize) -> Result<bool, String> {
        // The hook's first step at the next instruction: its fetch, for a
        // memory watch, even if an IRQ or a redirect then preempts it.
        if unsafe { (*self.state.get()).memory_watch.is_some() } {
            let pc = self.reg(core, arm::PC)?;
            self.record_memory(core, Access::Fetch, u64::from(pc), 4, 0);
            let state = unsafe { &mut *self.state.get() };
            state.fetch_mark = Some((core, pc, state.instructions));
        }
        self.due();
        let state = unsafe { &mut *self.state.get() };
        if state.failure.is_some() || state.cache_dirty {
            return Ok(true);
        }
        if state.gic.next_irq(core).is_some() && self.reg(core, arm::CPSR)? & 0x80 == 0 {
            state.irq_entries[core] += 1;
            let pc = self.reg(core, arm::PC)?;
            self.enter_exception(core, ExceptionEntry::Irq, pc)?;
            self.reset_edge(core);
            return Ok(true);
        }
        Ok(false)
    }
    /// Installs the return observers that labeled calls asked for.
    fn install_return_observers(&self, core: usize) -> Result<(), String> {
        let requests = std::mem::take(unsafe { &mut (*self.state.get()).return_requests });
        self.observe_addresses(core, &requests)
    }
    /// A board instruction that exited before entry: the per-instruction
    /// prologue has already run at this boundary. Returns `true` when it
    /// ends the slice (the hook's redirect).
    fn perform(&self, core: usize, word: u32) -> Result<bool, String> {
        self.reset_edge(core);
        let address = u64::from(self.reg(core, arm::PC)?);
        self.advance(core, 1)?;
        let state = unsafe { &mut *self.state.get() };
        state.last_observed[core] = Some((address as u32, state.instructions));
        observe(self, core, address)?;
        let state = unsafe { &mut *self.state.get() };
        state.redirected = None;
        state.keep_slice = false;
        special(self, core, address, word)?;
        let state = unsafe { &mut *self.state.get() };
        if state.failure.is_none() && std::mem::take(&mut state.keep_slice) {
            // Slice policy: the slice goes on after the performed instruction.
            if let Some(pc) = state.redirected.take() {
                self.write_reg(core, arm::PC, pc)?;
                return Ok(false);
            }
        }
        if state.redirected.is_some() || state.failure.is_some() {
            return Ok(true);
        }
        // The hook let the engine execute it: a failed condition, or a CCSIDR
        // or PMU access the board does not model. Do what the engine does.
        let cpsr = self.reg(core, arm::CPSR)?;
        if word >> 28 != 15 && !arm::condition_passes(word >> 28, cpsr) {
            self.write_reg(core, arm::PC, address as u32 + 4)?;
            return Ok(false);
        }
        if word & 0x0f00_0010 != 0x0e00_0010 || (word >> 8) & 15 != 15 {
            return Err(format!("Counted XP4 exit for an unperformed instruction {word:#010x} at {address:#x}"));
        }
        let rt = ((word >> 12) & 15) as usize;
        if rt == 15 {
            return Err(format!("Counted XP4 coprocessor transfer to APSR at {address:#x}"));
        }
        let selector = arm::Coprocessor {
            cp: 15,
            wide: false,
            secure: 0,
            opc1: (word >> 21) & 7,
            crn: (word >> 16) & 15,
            crm: word & 15,
            opc2: (word >> 5) & 7,
        };
        if word & (1 << 20) != 0 {
            let value = self.cpus[core].coprocessor(selector, None)?;
            self.write_reg(core, arm::CORE[rt], value as u32)?;
        } else {
            let value = self.reg(core, arm::CORE[rt])?;
            self.cpus[core].coprocessor(selector, Some(u64::from(value)))?;
        }
        self.write_reg(core, arm::PC, address as u32 + 4)?;
        Ok(false)
    }
}

impl Cluster {
    /// Forgets the predecessor block of `core` (board-performed instruction,
    /// IRQ or abort entry): what follows is not an edge.
    fn reset_edge(&self, core: usize) {
        if let Some(edges) = unsafe { (*self.state.get()).edges.as_mut() } {
            edges.last[core] = None;
        }
    }
}

/// Block observer of edge collection (counted runs, every block).
pub(super) unsafe fn edge_observer(block: fujimod_emulator_cpu::Block, data: *mut c_void) {
    let hook = unsafe { &*data.cast::<Hook>() };
    let c = unsafe { &*hook.cluster };
    let core = hook.core;
    let Some(edges) = (unsafe { (*c.state.get()).edges.as_mut() }) else {
        return;
    };
    let address = block.address as u32;
    let end = address.wrapping_add(block.bytes.saturating_sub(4));
    if let Some((start, last)) = edges.last[core] {
        // A retried (shortened) block re-enters at its own start; a cut block
        // continues inside its own span. Neither is a branch.
        if !(start <= address && address <= last.wrapping_add(4)) {
            let kind = match edges.kinds.get(&last) {
                Some(kind) => *kind,
                None => {
                    let kind = c.word(core, u64::from(last)).ok().and_then(indirect_kind);
                    edges.kinds.insert(last, kind);
                    kind
                }
            };
            if let Some(kind) = kind {
                edges.seen.entry((last, address)).or_insert((0, kind)).0 += 1;
            }
        }
    }
    edges.last[core] = Some((address, end));
}

impl Session {
    /// Starts collecting indirect-branch edges (counted runs): a block
    /// observer on every core, installed while the board is stopped.
    pub fn enable_indirect_edges(&self) -> Result<(), String> {
        let c = &self.inner;
        if !c.counted {
            return Err("Indirect edges need counted execution (FUJIMOD_XP4_COUNTED=1)".into());
        }
        if c.running.get() {
            return Err("Indirect edges are enabled between slices".into());
        }
        let state = unsafe { &mut *c.state.get() };
        if state.edges.is_some() {
            return Ok(());
        }
        state.edges = Some(Edges {
            last: vec![None; c.cpus.len()],
            ..Edges::default()
        });
        for core in 0..c.cpus.len() {
            let data = (&*c.hooks[core] as *const Hook).cast_mut().cast();
            unsafe {
                c.cpus[core].install_hook(fujimod_emulator_cpu::Hook::Block {
                    callback: edge_observer,
                    data,
                    range: HookRange::All,
                })
            }?;
        }
        Ok(())
    }
    /// Every edge seen so far: (branch address, target, entries, kind).
    pub fn indirect_edges(&self) -> Vec<(u32, u32, u64, &'static str)> {
        unsafe { (*self.inner.state.get()).edges.as_ref() }
            .map(|e| e.seen.iter().map(|(&(s, d), &(n, k))| (s, d, n, k)).collect())
            .unwrap_or_default()
    }
    /// Starts recording every instruction the counted engines translate:
    /// each is entered code, so the log is the coverage of new code.
    pub fn enable_coverage(&self) -> Result<(), String> {
        if !self.inner.counted {
            return Err("Coverage needs counted execution (FUJIMOD_XP4_COUNTED=1)".into());
        }
        for log in &self.inner.translation_logs {
            log.borrow_mut().get_or_insert_with(Vec::new);
        }
        Ok(())
    }
    /// Drains the addresses translated since the last call, all cores,
    /// sorted and without duplicates.
    pub fn take_coverage(&self) -> Result<Vec<u64>, String> {
        let mut all = Vec::new();
        for log in &self.inner.translation_logs {
            let mut log = log.borrow_mut();
            let log = log
                .as_mut()
                .ok_or("Coverage is not enabled (enable_coverage)")?;
            all.append(log);
        }
        all.sort_unstable();
        all.dedup();
        Ok(all)
    }
    /// One scheduler slice of at most `budget` entered instructions, ended
    /// where the per-instruction hook ends it.
    pub(super) fn run_counted(&self, core: usize, budget: usize) -> Result<(), String> {
        let c = &self.inner;
        let mut remaining = budget as u64;
        let mut exit_word: Option<u32> = None;
        let result = (|| -> Result<(), String> {
            loop {
                // Unicorn's instruction counter is the first code hook: once
                // the slice is spent it stops before the board's hook runs
                // for the next instruction, so no boundary work happens then.
                if remaining == 0 {
                    return Ok(());
                }
                if c.boundary(core)? {
                    return Ok(());
                }
                if let Some(word) = exit_word.take() {
                    remaining -= 1;
                    if c.perform(core, word)? {
                        return Ok(());
                    }
                    continue;
                }
                c.install_return_observers(core)?;
                let room = c.room(core)?;
                if room == 0 {
                    return Err("Counted XP4 boundary left a deadline due".into());
                }
                let limit = remaining.min(room);
                let cpu = &c.cpus[core];
                let pending = unsafe { (*c.state.get()).gic.next_irq(core).is_some() };
                cpu.set_interrupt(Interrupt::Irq, pending)?;
                let before = cpu.exception_entries().unwrap_or_default();
                unsafe {
                    (*c.state.get()).accounted = 0;
                    (*c.state.get()).counted_core = Some(core);
                }
                let begin = u64::from(c.reg(core, arm::PC)?);
                let exit = cpu.run(Run {
                    begin,
                    until: None,
                    watchdog_us: 0,
                    instructions: Some(limit as usize),
                });
                unsafe {
                    (*c.state.get()).counted_core = None;
                }
                let entered = cpu.instruction_progress().unwrap_or(0);
                let accounted = unsafe { (*c.state.get()).accounted };
                c.advance(core, entered.saturating_sub(accounted))?;
                remaining = remaining.saturating_sub(entered);
                let after = cpu.exception_entries().unwrap_or_default();
                if after.prefetch_abort != before.prefetch_abort
                    || after.data_abort != before.data_abort
                {
                    let state = unsafe { &mut *c.state.get() };
                    state.prefetch_aborts[core] += after.prefetch_abort - before.prefetch_abort;
                    state.data_aborts[core] += after.data_abort - before.data_abort;
                    state.exclusive[core] = None;
                    c.reset_edge(core);
                    return Ok(());
                }
                if after.irq != before.irq {
                    return Err("Counted XP4 engine entered an IRQ itself".into());
                }
                match exit {
                    Ok(Exit::BeforeInstruction(word)) => exit_word = Some(word),
                    Ok(Exit::Stopped) => {}
                    Ok(other) => {
                        return Err(format!("Unexpected counted ARMv7-A exit {other:?}"));
                    }
                    Err(ExecutionError::InvalidInstruction(text)) => {
                        if unsafe { (*c.state.get()).failure.is_some() } {
                            return Ok(());
                        }
                        // As the hook: the engine refused a VFP instruction the
                        // guest disabled; enter its undefined exception.
                        return match c.disabled_vfp_exception(core) {
                            Ok(true) => Ok(()),
                            Ok(false) => Err(ExecutionError::InvalidInstruction(text).to_string()),
                            Err(error) => Err(error),
                        };
                    }
                    Err(error) => return Err(error.to_string()),
                }
                if unsafe { (*c.state.get()).failure.is_some() } {
                    return Ok(());
                }
            }
        })();
        if let Err(error) = result
            && unsafe { (*c.state.get()).failure.is_none() }
        {
            unsafe {
                (*c.state.get()).failure = Some(error);
            }
        }
        Ok(())
    }
}

/// ARM-state LDREX/STREX family and CLREX (the counted engines' owner exits).
fn exclusive_word(word: u32) -> bool {
    word & 0x0f900fff == 0x01900f9f || word & 0x0f900ff0 == 0x01800f90 || word == 0xf57ff01f
}

/// Counted observer (labels, call returns, recorder addresses, watched code
/// ranges): what the per-instruction hook does at an entered instruction,
/// i.e. its fetch record for a memory watch (before the instruction counts),
/// then the passive observations (after it counts).
pub(super) unsafe fn counted_observer(address: u64, _size: u32, data: *mut c_void) {
    let hook = unsafe { &*data.cast::<Hook>() };
    let c = unsafe { &*hook.cluster };
    let core = hook.core;
    let result = (|| -> Result<(), String> {
        // A board instruction exits right after this callback without being
        // entered; `perform` records and observes it at the boundary.
        let word = c.word(core, address)?;
        if fujimod_emulator_cpu::armv7a::framework_instruction(word) || exclusive_word(word) {
            return Ok(());
        }
        let progress = c.cpus[core]
            .instruction_progress()
            .ok_or("counted XP4 observer without an entry count")?;
        let state = unsafe { &mut *c.state.get() };
        let entered = state.instructions + (progress - state.accounted);
        // A restarted instruction or overlapping observers: once per entry.
        if state.last_observed[core] == Some((address as u32, entered)) {
            return Ok(());
        }
        state.last_observed[core] = Some((address as u32, entered));
        if state.memory_watch.is_some() {
            c.sync_to(core, progress - 1)?;
            let state = unsafe { &mut *c.state.get() };
            let mark = state.fetch_mark.take();
            if mark != Some((core, address as u32, state.instructions)) {
                c.record_memory(core, Access::Fetch, address, 4, 0);
            }
        }
        c.sync_to(core, progress)?;
        let before = unsafe { (*c.state.get()).calls.len() };
        observe(c, core, address)?;
        let state = unsafe { &mut *c.state.get() };
        if state.calls.len() != before {
            let lr = state.calls[before].lr;
            if !state.observed[core].contains(&lr) {
                // Install its return observer at the next boundary, before
                // the callee can return.
                state.return_requests.push(lr);
                c.cpus[core].tighten_instruction_limit(state.accounted)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        c.fail(core, error);
    }
}
