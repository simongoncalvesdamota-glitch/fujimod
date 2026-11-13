//! ARM application-profile cluster alongside the shared R-profile runtime.
//! The typed CPU backend, backing owner and discovery ledger are shared.
mod abort;
mod client;
mod counted;
mod definition;
pub mod memory;
pub use definition::Definition;
pub mod observation;
use observation::Call;
mod devices;
mod lens_motion;
mod timer;
use fujimod_emulator::board::discovery::Discovery;
use fujimod_emulator::board::harness::arm32 as driver_guard;
use fujimod_emulator::board::observation::calls as observe;
use fujimod_emulator_cpu::{
    self as cpu, Access, Cpu, ExecutionError, Exit, Fault, Hook as CpuHook, HookRange, Register,
    Run, arm, memory::Backing,
};
use fujimod_emulator_cpu_unicorn::Library;
use fujimod_emulator_ip_arm::gicv2::Gic;
use std::{
    cell::{Cell, RefCell, UnsafeCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::c_void,
    ptr::NonNull,
    rc::Rc,
};

const FPEXC: Register = Register::Arm32(arm::Register::Fpexc);

enum ExceptionEntry {
    Undefined,
    Irq,
    PrefetchAbort,
    DataAbort,
}

struct Running<'a>(&'a Cell<bool>);
impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

struct Hook {
    cluster: *const Cluster,
    core: usize,
}
struct MmioHook {
    cluster: *const Cluster,
    core: usize,
    base: u64,
}
/// One guest MMIO access seen by [`Session::mmio_trace_begin`]: the value
/// written, or for a read the value the board replied.
#[derive(Clone, Debug, serde::Serialize)]
pub struct MmioWrite {
    pub read: bool,
    pub ticks: u64,
    pub instructions: u64,
    pub core: usize,
    pub pc: u64,
    pub address: u64,
    pub size: u32,
    pub value: u64,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MmioTraceSnapshot {
    pub ranges: Vec<(u64, u64)>,
    pub limit: usize,
    pub writes: Vec<MmioWrite>,
    // Accesses inside the ranges that came after `limit` was reached.
    pub lost: u64,
}
struct MmioTrace {
    ranges: Vec<(u64, u64)>,
    limit: usize,
    // Also log reads (off: writes only).
    reads: bool,
    writes: Vec<MmioWrite>,
    lost: u64,
}
struct RunState {
    instructions: u64,
    ticks: u64,
    tick_phase: u64,
    per_core: Vec<u64>,
    waiting: Vec<u8>, // 0 runnable, 1 WFI, 2 WFE
    events: Vec<bool>,
    recent: Vec<VecDeque<(u32, u32)>>,
    milestones: BTreeMap<(usize, u32), u64>,
    discovery: Discovery,
    failure: Option<String>,
    redirected: Option<u32>,
    // Set by `step_over` when the slice policy keeps the slice past a
    // board-performed exclusive or identity read; consumed by `run`.
    keep_slice: bool,
    pages: BTreeMap<u64, Vec<Box<MmioHook>>>,
    cbar_reads: u64,
    gic: Gic,
    timer: Option<timer::Timer>,
    irq_entries: Vec<u64>,
    undefined_entries: Vec<u64>,
    prefetch_aborts: Vec<u64>,
    data_aborts: Vec<u64>,
    calls: Vec<Call>,
    pending_calls: Vec<BTreeMap<u32, Vec<usize>>>,
    // Addresses that may need a passive observation (see `mark_observed`).
    observe_filter: Vec<u64>,
    // Counted execution: entries of the current run already accounted, the
    // core running it, and single-address call-return observers.
    accounted: u64,
    counted_core: Option<usize>,
    return_requests: Vec<u32>,
    // Addresses with a single-address counted observer (labels, call
    // returns, recorder entries/returns), per core.
    observed: Vec<BTreeSet<u32>>,
    // Last observed (address, entered instruction count) per core: a
    // restarted instruction or two overlapping observers report it once.
    last_observed: Vec<Option<(u32, u64)>>,
    // Fetch recorded at a boundary for (core, pc, instructions before it).
    fetch_mark: Option<(usize, u32, u64)>,
    // Indirect-branch edges observed in counted runs (`counted.rs`).
    edges: Option<counted::Edges>,
    exclusive: Vec<Option<(u64, usize)>>,
    recorder: Option<observe::Recorder>,
    memory_watch: Option<observation::MemoryWatch>,
    // Passive log of guest MMIO writes inside chosen ranges (observation
    // only: it never changes a reply, a device or the schedule).
    mmio_trace: Option<MmioTrace>,
    pc_observers: Vec<BTreeSet<u64>>,
    devices: Option<devices::Devices>,
    cache_dirty: bool,
    pmu: Vec<Option<fujimod_emulator::board::armv7a::Profile>>,
    // Memory window shown at physical 0 until the guest selects the remap.
    low_alias: Option<usize>,
    remap_pending: bool,
    remap_ready: bool,
    driver_guard: Option<driver_guard::Guard>,
}

struct Cluster {
    // Destroy processors before their RAM backing and callback contexts.
    cpus: Vec<Box<dyn Cpu>>,
    ram: Backing,
    dram_alias: Option<u64>,
    memory: Vec<memory::Window>,
    hooks: Vec<Box<Hook>>,
    labels: Vec<u32>,
    // Counted native-block execution (`counted.rs`) instead of the
    // per-instruction hook.
    counted: bool,
    // Per-core logs of translated instruction addresses (counted runs).
    translation_logs: Vec<fujimod_emulator_cpu_unicorn::aprofile::TranslationLog>,
    cbar: Option<u32>,
    timer_base: Option<u32>,
    cache_ids: Option<[u32; 3]>,
    instruction_rate: Cell<fujimod_emulator_cpu::timeline::Ratio>,
    per_core_time: Cell<bool>,
    // Slice policy (off by default): exclusives, CLREX and identity reads
    // no longer end the scheduler slice; only WFI/WFE/SEV do.
    through_exclusives: Cell<bool>,
    running: Cell<bool>,
    pc_handler: RefCell<Option<InstructionObserver>>,
    state: UnsafeCell<RunState>,
}
impl Cluster {
    // Passive MMIO trace (`Session::mmio_trace_begin`): never changes state
    // other than its own log.
    fn trace_mmio(&self, core: usize, address: u64, size: u32, write: bool, value: u64) {
        let state = unsafe { &mut *self.state.get() };
        let (ticks, instructions) = (state.ticks, state.instructions);
        let Some(trace) = state.mmio_trace.as_mut() else {
            return;
        };
        if !(write || trace.reads) || !trace.ranges.iter().any(|&(lo, hi)| lo <= address && address < hi) {
            return;
        }
        if trace.writes.len() >= trace.limit {
            trace.lost += 1;
            return;
        }
        let pc = self.reg(core, arm::PC).map(u64::from).unwrap_or(0);
        trace.writes.push(MmioWrite { read: !write, ticks, instructions, core, pc, address, size, value });
    }
    fn record_memory(&self, core: usize, access: Access, address: u64, size: i32, value: i64) {
        if unsafe { (*self.state.get()).memory_watch.is_none() } {
            return;
        }
        let result = (|| -> Result<Option<observation::MemoryRecord>, String> {
            let bytes = usize::try_from(size).map_err(|_| "invalid observed memory width")?;
            if bytes == 0 {
                return Err("invalid observed memory width".into());
            }
            let physical = match self.cpus[core].translate(address, access) {
                Ok(physical) => physical,
                Err(_) => return Ok(None),
            };
            if physical.saturating_add(bytes as u64) > self.ram.len() as u64 {
                return Ok(None);
            }
            let (instructions, tick, in_range) = {
                let state = unsafe { &*self.state.get() };
                let Some(watch) = state.memory_watch.as_ref() else {
                    return Ok(None);
                };
                (
                    state.instructions,
                    state.ticks,
                    watch.overlaps(physical, bytes),
                )
            };
            if !in_range {
                return Ok(None);
            }
            if !(1..=16).contains(&bytes) {
                return Err("Memory watch cannot capture this CPU access width".into());
            }
            let byte_order = if self.reg(core, arm::CPSR)? & (1 << 9) != 0 {
                "big"
            } else {
                "little"
            };
            let before = if access == Access::Fetch {
                None
            } else {
                let mut data = vec![0; bytes];
                self.cpus[core].read_memory(address, &mut data)?;
                Some(data)
            };
            let observed_value = match access {
                Access::Fetch => None,
                Access::Write if bytes <= 8 => {
                    let mask = u64::MAX >> (64 - bytes.min(8) * 8);
                    Some(value as u64 & mask)
                }
                Access::Read if bytes <= 8 => {
                    let data = before.as_ref().unwrap();
                    Some(if byte_order == "little" {
                        data.iter()
                            .rev()
                            .fold(0u64, |v, b| (v << 8) | u64::from(*b))
                    } else {
                        data.iter().fold(0u64, |v, b| (v << 8) | u64::from(*b))
                    })
                }
                _ => None,
            };
            Ok(Some(observation::MemoryRecord {
                source: match access {
                    Access::Fetch => "cpu-fetch",
                    Access::Read => "cpu-read",
                    Access::Write => "cpu-write",
                },
                address,
                physical,
                size: bytes,
                value: observed_value,
                before,
                byte_order,
                core,
                pc: self.reg(core, arm::PC)?,
                instructions,
                tick,
            }))
        })();
        let state = unsafe { &mut *self.state.get() };
        if let Some(watch) = state.memory_watch.as_mut() {
            match result {
                Ok(Some(record)) => watch.push(record),
                Ok(None) => {}
                Err(error) => {
                    watch.violation.get_or_insert(error);
                }
            }
        }
    }

    fn external_devices<T>(
        &self,
        action: impl FnOnce(&mut devices::Devices, u64) -> Result<T, String>,
    ) -> Result<T, String> {
        if unsafe { (*self.state.get()).driver_guard.is_some() } {
            return Err("Guarded routine cannot change external device inputs".into());
        }
        self.devices_due()?;
        let state = unsafe { &mut *self.state.get() };
        let now = state.ticks;
        let mut devices = state.devices.take().ok_or("A7 devices are not wired")?;
        let result = action(&mut devices, now);
        devices.changed();
        unsafe {
            (*self.state.get()).devices = Some(devices);
        }
        result
    }
    fn cpu(&self, core: usize) -> Result<&dyn Cpu, String> {
        self.cpus
            .get(core)
            .map(|cpu| &**cpu)
            .ok_or_else(|| "Invalid ARMv7-A core".to_owned())
    }
    fn reg(&self, core: usize, register: Register) -> Result<u32, String> {
        let mut bytes = [0; 4];
        let size = self.cpus[core].read_register(register, &mut bytes)?;
        if size != bytes.len() {
            return Err("ARMv7-A scalar register is not 32 bits".into());
        }
        Ok(u32::from_le_bytes(bytes))
    }
    fn write_reg(&self, core: usize, register: Register, value: u32) -> Result<(), String> {
        self.cpus[core].write_register(register, &value.to_le_bytes())
    }
    fn cp(&self, core: usize, crn: u32, crm: u32, opc1: u32, opc2: u32) -> Result<u64, String> {
        if (crn, crm, opc1, opc2) == (0, 0, 1, 0)
            && let Some(ids) = self.cache_ids
        {
            let selector = self.cp(core, 0, 0, 2, 0)? as usize;
            return ids
                .get(selector)
                .map(|&value| u64::from(value))
                .ok_or_else(|| "Reserved Cortex-A7 cache selector".into());
        }
        self.cpus[core].coprocessor(
            arm::Coprocessor {
                cp: 15,
                wide: false,
                secure: 0,
                crn,
                crm,
                opc1,
                opc2,
            },
            None,
        )
    }
    fn fail(&self, core: usize, message: String) {
        unsafe {
            keep_first_failure(&mut (*self.state.get()).failure, message);
            let _ = self.cpus[core].stop();
        }
    }
    fn redirect(&self, core: usize, pc: u32) -> Result<(), String> {
        unsafe {
            (*self.state.get()).redirected = Some(pc);
        }
        self.write_reg(core, arm::PC, pc)?;
        self.cpus[core].stop()
    }
    /// `redirect` for an exclusive, CLREX or identity read: under the
    /// `through_exclusives` slice policy the scheduler slice goes on after it.
    fn step_over(&self, core: usize, pc: u32) -> Result<(), String> {
        if self.through_exclusives.get() {
            unsafe {
                (*self.state.get()).keep_slice = true;
            }
        }
        self.redirect(core, pc)
    }
    fn word(&self, core: usize, address: u64) -> Result<u32, String> {
        // The engine is about to execute this word, so its fetch permission
        // already holds: when it translates into plain DRAM, read the backing
        // directly instead of the observational read (page-size query, region
        // copy and physical read on every instruction). Explicit windows lie
        // above DRAM; only the low boot window overlaps it until the remap.
        if let Ok(physical) = self.cpus[core].translate(address, Access::Fetch) {
            let low = unsafe { (*self.state.get()).low_alias }
                .map_or(0, |index| self.memory[index].data.len() as u64);
            if physical >= low && physical & 3 == 0 && physical + 4 <= self.ram.len() as u64 {
                return Ok(u32::from_le(unsafe {
                    std::ptr::read(self.ram.as_ptr().add(physical as usize).cast::<u32>())
                }));
            }
        }
        let mut bytes = [0; 4];
        self.cpus[core].read_virtual(address, Access::Fetch, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn inspect_ram_word(&self, core: usize, address: u32) -> Result<u32, String> {
        let physical = self.cpus[core].translate(u64::from(address), Access::Read)?;
        if physical + 4 > self.ram.len() as u64 {
            return Err("Passive observer stack outside RAM".into());
        }
        Ok(unsafe {
            std::ptr::read_unaligned(self.ram.as_ptr().add(physical as usize).cast::<u32>())
        })
    }
    fn physical_ram(
        &self,
        core: usize,
        address: u32,
        size: usize,
        access: Access,
    ) -> Result<u64, String> {
        let physical = self.cpus[core].translate(u64::from(address), access)?;
        if address as usize & (size - 1) != 0 || physical + size as u64 > self.ram.len() as u64 {
            return Err("ARMv7-A exclusive outside aligned physical RAM".into());
        }
        Ok(physical)
    }
    fn due(&self) {
        if let Err(error) = self.devices_due() {
            unsafe {
                keep_first_failure(&mut (*self.state.get()).failure, error);
                for cpu in &self.cpus {
                    let _ = cpu.stop();
                }
            }
            return;
        }
        let state = unsafe { &mut *self.state.get() };
        // A counter expires only once its next tick is reached (`tick` is a
        // no-op before), so skip the channel walk until then.
        if let Some(timer) = &mut state.timer
            && timer.next_event_tick(state.ticks).is_some_and(|at| at <= state.ticks)
        {
            for irq in timer.tick(state.ticks) {
                state.gic.pulse(irq);
            }
        }
        for core in 0..self.cpus.len() {
            if state.gic.next_irq(core).is_some() {
                state.waiting[core] = 0;
            }
        }
    }
    fn enter_exception(
        &self,
        core: usize,
        exception: ExceptionEntry,
        pc: u32,
    ) -> Result<(), String> {
        let old = self.reg(core, arm::CPSR)?;
        let sctlr = self.cp(core, 1, 0, 0, 0)? as u32;
        if old & 0x20 != 0 || sctlr & ((1 << 30) | (1 << 25)) != 0 {
            return Err("Unsupported Thumb/big-endian ARMv7-A exception entry".into());
        }
        let base = if sctlr & (1 << 13) != 0 {
            0xffff0000
        } else {
            self.cp(core, 12, 0, 0, 0)? as u32 & !31
        };
        let (mode, offset, return_offset, masks) = match exception {
            ExceptionEntry::Undefined => (0x1b, 4, 4, 0x80),
            ExceptionEntry::Irq => (0x12, 0x18, 4, 0x180),
            ExceptionEntry::PrefetchAbort => (0x17, 0x0c, 4, 0x180),
            ExceptionEntry::DataAbort => (0x17, 0x10, 8, 0x180),
        };
        let new = (old & !(0x1f | 0x20 | 0x0600fc00 | 0x01000000 | 0x200)) | mode | masks;
        self.write_reg(core, arm::CPSR, new)?;
        self.write_reg(core, arm::SPSR, old)?;
        self.write_reg(core, arm::CORE[14], pc.wrapping_add(return_offset))?;
        unsafe {
            (&mut (*self.state.get()).exclusive)[core] = None;
        }
        self.redirect(core, base + offset)
    }
    fn disabled_vfp_exception(&self, core: usize) -> Result<bool, String> {
        let pc = self.reg(core, arm::PC)?;
        let word = self.word(core, u64::from(pc))?;
        // Conditional ARM coprocessor-10/11 instructions (including VPUSH),
        // and the unconditional Advanced SIMD data-processing (0xf2/0xf3) and
        // element/structure load/store (0xf4 with bit 20 clear) encodings,
        // which share the CPACR/FPEXC gate. Other invalid/unsupported
        // instructions retain the engine refusal.
        let simd = word & 0xfe00_0000 == 0xf200_0000 || word & 0xff10_0000 == 0xf400_0000;
        if !simd && (word >> 28 == 15 || word & 0x0c000e00 != 0x0c000a00) {
            return Ok(false);
        }
        let cpsr = self.reg(core, arm::CPSR)?;
        let cpacr = self.cp(core, 1, 0, 0, 2)? as u32;
        let privileged = cpsr & 31 != 0x10;
        let accessible = [20, 22].into_iter().all(|shift| {
            let access = (cpacr >> shift) & 3;
            access == 3 || (access == 1 && privileged)
        });
        if accessible && self.reg(core, FPEXC)? & 0x40000000 != 0 {
            return Ok(false);
        }
        self.enter_exception(core, ExceptionEntry::Undefined, pc)?;
        unsafe {
            (&mut (*self.state.get()).undefined_entries)[core] += 1;
        }
        Ok(true)
    }
    fn map_page(&self, address: u64) -> Result<(), String> {
        let page = address & !0xfff;
        if unsafe { (*self.state.get()).pages.contains_key(&page) } {
            return Ok(());
        }
        unsafe {
            (*self.state.get()).pages.insert(page, Vec::new());
        }
        for core in 0..self.cpus.len() {
            let hook = Box::new(MmioHook {
                cluster: self,
                core,
                base: page,
            });
            let ptr = (&*hook as *const MmioHook).cast_mut().cast();
            // Retain the context even if a later CPU refuses the map.
            unsafe {
                (*self.state.get()).pages.get_mut(&page).unwrap().push(hook);
                self.cpus[core].map_mmio(
                    page,
                    4096,
                    cpu::Mmio {
                        read: mmio_read,
                        write: mmio_write,
                        data: ptr,
                    },
                )
            }
            .map_err(|e| format!("Map discovered page {page:#x} core{core}: {e}"))?;
        }
        Ok(())
    }
}

// CPU callbacks use the typed backend boundary. Decoding is architectural;
// devices and the discovery ledger receive only accesses and virtual time.
unsafe fn code(address: u64, size: u32, data: *mut c_void) {
    let hook = unsafe { &*data.cast::<Hook>() };
    let cluster = unsafe { &*hook.cluster };
    if let Err(error) = instruction(cluster, hook.core, address, size) {
        cluster.fail(hook.core, error);
    }
}
unsafe fn memory_access(access: Access, address: u64, size: i32, value: i64, data: *mut c_void) {
    let hook = unsafe { &*data.cast::<Hook>() };
    let cluster = unsafe { &*hook.cluster };
    // Counted runs: guest time up to this instruction, as the hook sees it.
    if unsafe { (*cluster.state.get()).counted_core } == Some(hook.core)
        && let Err(error) = cluster.sync_counted(hook.core)
    {
        cluster.fail(hook.core, error);
        return;
    }
    cluster.record_memory(hook.core, access, address, size, value);
}
/// Marks an address in the 64 Ki-bit passive-observation filter, keyed by
/// instruction-address bits 2..18. Bits are never cleared: a set bit only
/// sends the instruction through the exact label/return/observer lookups.
fn mark_observed(filter: &mut [u64], address: u64) {
    let key = (address as u32 >> 2) & 0xffff;
    filter[(key >> 6) as usize] |= 1 << (key & 63);
}
fn instruction(c: &Cluster, core: usize, address: u64, size: u32) -> Result<(), String> {
    if unsafe { (*c.state.get()).memory_watch.is_some() } {
        c.record_memory(core, Access::Fetch, address, size as i32, 0);
    }
    if let Some(guard) = unsafe { &mut (*c.state.get()).driver_guard } {
        // Check architecture before a return sentinel or IRQ can bypass the
        // routine boundary (e.g. MOVS PC,LR restoring an unmasked SPSR).
        let cpsr = c.reg(core, arm::CPSR)?;
        let admitted = if cpsr & (0x20 | 0x200) != 0 || cpsr & 0xc0 != 0xc0 {
            Err("Driver guard requires little-endian ARM state with IRQ/FIQ masked".into())
        } else if c.cp(core, 1, 0, 0, 0)? & ((1 << 25) | 1) != 0 {
            Err("Driver guard requires MMU-off little-endian execution".into())
        } else {
            guard.fetch(core, address, size)
        };
        if let Err(error) = admitted.as_ref() {
            guard.failure = Some(error.clone());
        }
        if admitted != Ok(true) {
            c.cpus[core].stop()?;
            return Ok(());
        }
    }
    if size != 4 {
        return Err(format!(
            "Unimplemented ARMv7-A Thumb instrumentation at {address:#x}"
        ));
    }
    c.due();
    if unsafe {
        (*c.state.get())
            .driver_guard
            .as_ref()
            .is_some_and(|guard| guard.stopped())
    } {
        c.cpus[core].stop()?;
        return Ok(());
    }
    if unsafe { (*c.state.get()).remap_pending } && c.cp(core, 1, 0, 0, 0)? & 1 == 0 {
        unsafe {
            (*c.state.get()).remap_ready = true;
            (*c.state.get()).cache_dirty = true;
        }
    }
    if unsafe { (*c.state.get()).cache_dirty } {
        return c.redirect(core, address as u32);
    }
    if let Some(error) = unsafe { (*c.state.get()).failure.as_ref() } {
        return Err(error.clone());
    }
    if unsafe { (*c.state.get()).gic.next_irq(core).is_some() }
        && c.reg(core, arm::CPSR)? & 0x80 == 0
    {
        unsafe {
            (&mut (*c.state.get()).irq_entries)[core] += 1;
        }
        return c.enter_exception(core, ExceptionEntry::Irq, address as u32);
    }
    // Instruction inspection uses Unicorn's architectural MMU translation too.
    let word = c.word(core, address)?;
    if let Some(guard) = unsafe { &mut (*c.state.get()).driver_guard } {
        let result = guard.preflight(address, word, c.reg(core, arm::CPSR)?, |index| {
            c.reg(core, arm::CORE[index])
        });
        if let Err(error) = result {
            guard.failure = Some(error);
            c.cpus[core].stop()?;
            return Ok(());
        }
    }
    c.advance(core, 1)?;
    let state = unsafe { &mut *c.state.get() };
    let trace = &mut state.recent[core];
    if trace.len() == 32 {
        trace.pop_front();
    }
    trace.push_back((address as u32, word));
    observe(c, core, address)?;
    special(c, core, address, word)
}
/// Passive per-instruction observations at an entered instruction: labels
/// (milestones and calls), call returns, the scalar recorder and explicit
/// PC observers. They never select device or CPU behavior.
fn observe(c: &Cluster, core: usize, address: u64) -> Result<(), String> {
    let state = unsafe { &mut *c.state.get() };
    // Passive call/return observations cannot select device or CPU behavior.
    // The filter holds every label, pending return and PC observer address
    // (never cleared, so a set bit only means "look it up").
    let key = (address as u32 >> 2) & 0xffff;
    let candidate = state.observe_filter[(key >> 6) as usize] & (1 << (key & 63)) != 0;
    let labeled = candidate && c.labels.contains(&(address as u32));
    let returning = candidate && state.pending_calls[core].contains_key(&(address as u32));
    let sp = if labeled || returning {
        c.reg(core, arm::CORE[13])?
    } else {
        0
    };
    if returning && let Some(indices) = state.pending_calls[core].get_mut(&(address as u32)) {
        indices.retain(|&index| {
            let call = &mut state.calls[index];
            if call.sp == sp {
                call.result = c.reg(core, arm::CORE[0]).ok();
                false
            } else {
                true
            }
        });
        if indices.is_empty() {
            state.pending_calls[core].remove(&(address as u32));
        }
    }
    if labeled {
        state
            .milestones
            .entry((core, address as u32))
            .or_insert(state.instructions);
        if state.calls.len() < 4096 {
            let mut args = Vec::new();
            for reg in &arm::CORE[..4] {
                args.push(c.reg(core, *reg)?);
            }
            for offset in (0..24).step_by(4) {
                args.push(
                    c.inspect_ram_word(core, sp.wrapping_add(offset))
                        .unwrap_or(0),
                );
            }
            let index = state.calls.len();
            let lr = c.reg(core, arm::CORE[14])?;
            state.calls.push(Call {
                core,
                pc: address as u32,
                instructions: state.instructions,
                lr,
                sp,
                args,
                result: None,
            });
            state.pending_calls[core].entry(lr).or_default().push(index);
            mark_observed(&mut state.observe_filter, u64::from(lr));
        }
    }
    if let Some(recorder) = &mut state.recorder
        && recorder.watches(address)
    {
        let mut registers = [0; 16];
        for (i, register) in registers.iter_mut().enumerate().take(15) {
            *register = c.reg(core, arm::CORE[i])?;
        }
        registers[15] = c.reg(core, arm::CPSR)?;
        recorder.observe(
            core,
            address,
            state.instructions,
            registers,
            u64::from(registers[14]) & !1,
        );
    }
    let observed = candidate && state.pc_observers[core].contains(&address);
    // The actual return is already recorded. Explicit discovery/debugger
    // callbacks run at the CPU boundary, never inside a peripheral model.
    if observed {
        let callback = c.pc_handler.borrow().as_ref().cloned();
        if let Some(callback) = callback {
            callback(core, address)?;
        }
        if c.reg(core, arm::PC)? != address as u32 {
            return Err("ARMv7-A PC observers may inspect or change registers, but cannot redirect execution".into());
        }
    }
    Ok(())
}
/// The instructions the board performs itself (PMU profile, exclusives,
/// CLREX, identity reads, WFI/WFE/SEV), at an entered instruction whose
/// word is `word`. Anything else returns without effect, for the engine
/// to execute.
fn special(c: &Cluster, core: usize, address: u64, word: u32) -> Result<(), String> {
    let state = unsafe { &mut *c.state.get() };
    // PMCR, PMCNTENSET/CLR and PMCCNTR follow the shared ARMv7-A PMU profile.
    if word & 0x0fef0f10 == 0x0e090f10
        && matches!(word & 15, 12 | 13)
        && let Some(pmu) = state.pmu.get_mut(core).and_then(Option::as_mut)
    {
        use fujimod_emulator::board::armv7a::Entry;
        if !matches!(
            pmu.before(&*c.cpus[core], address, state.ticks)?,
            Entry::Execute
        ) {
            state.redirected = Some(address as u32 + 4);
            return Ok(());
        }
    }
    let load_exclusive = word & 0x0f900fff == 0x01900f9f;
    let store_exclusive = word & 0x0f900ff0 == 0x01800f90;
    let cp_read = word & 0x0fff0fff;
    let hint = word & 0x0fffffff;
    if !load_exclusive
        && !store_exclusive
        && word != 0xf57ff01f
        && cp_read != 0x0e100fb0
        && cp_read != 0x0e9f0f10
        && !(cp_read == 0x0e300f10 && c.cache_ids.is_some())
        && !matches!(hint, 0x0320f002 | 0x0320f003 | 0x0320f004)
    {
        return Ok(());
    }
    let condition = word >> 28;
    let flags = c.reg(core, arm::CPSR)?;
    let (n, z, carry, v) = (
        flags >> 31 != 0,
        flags & (1 << 30) != 0,
        flags & (1 << 29) != 0,
        flags & (1 << 28) != 0,
    );
    let passed = match condition {
        0 => z,
        1 => !z,
        2 => carry,
        3 => !carry,
        4 => n,
        5 => !n,
        6 => v,
        7 => !v,
        8 => carry && !z,
        9 => !carry || z,
        10 => n == v,
        11 => n != v,
        12 => !z && n == v,
        13 => z || n != v,
        _ => true,
    };
    if !passed {
        return Ok(());
    }
    if load_exclusive || store_exclusive {
        let rn = ((word >> 16) & 15) as usize;
        let rd = ((word >> 12) & 15) as usize;
        if rn == 15 || rd == 15 {
            return Err("Unpredictable ARMv7-A exclusive register".into());
        }
        let load = load_exclusive;
        let size = match (word >> 21) & 3 {
            0 => 4,
            1 => 8,
            2 => 1,
            _ => 2,
        };
        let data_address = c.reg(core, arm::CORE[rn])?;
        let physical = match c.physical_ram(
            core,
            data_address,
            size,
            if load { Access::Read } else { Access::Write },
        ) {
            Ok(physical) => physical,
            Err(error) => {
                return c
                    .deliver_abort(core, false)
                    .map_err(|abort| format!("{error}: {abort}"));
            }
        };
        if load {
            if size == 8 && (rd & 1 != 0 || rd > 12) {
                return Err("Unpredictable LDREXD register pair".into());
            }
            let mut bytes = [0u8; 8];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    c.ram.as_ptr().add(physical as usize),
                    bytes.as_mut_ptr(),
                    size,
                );
            }
            let value = u64::from_le_bytes(bytes);
            c.write_reg(core, arm::CORE[rd], value as u32)?;
            if size == 8 {
                c.write_reg(core, arm::CORE[rd + 1], (value >> 32) as u32)?;
            }
            state.exclusive[core] = Some((physical, size));
            if c.counted {
                // Route every core's stores to this monitor through `store`.
                for cpu in &c.cpus {
                    cpu.watch_stores(physical, size as u64)?;
                }
            }
        } else {
            let success = state.exclusive[core] == Some((physical, size));
            state.exclusive[core] = None;
            if success {
                let rt = (word & 15) as usize;
                if rt == 15 {
                    return Err("Unpredictable ARMv7-A STREX Rt".into());
                }
                if size == 8 && (rt & 1 != 0 || rt > 12) {
                    return Err("Unpredictable STREXD register pair".into());
                }
                let mut value = u64::from(c.reg(core, arm::CORE[rt])?);
                if size == 8 {
                    value |= u64::from(c.reg(core, arm::CORE[rt + 1])?) << 32;
                }
                for monitor in &mut state.exclusive {
                    if monitor
                        .is_some_and(|(p, n)| p < physical + size as u64 && physical < p + n as u64)
                    {
                        *monitor = None;
                    }
                }
                c.cpus[core].write_memory(physical, &value.to_le_bytes()[..size])?;
                // Software-emulated exclusives bypass the CPU memory hook.
                // Preflight checked the complete RAM write and record bound;
                // record only the store that actually succeeded.
                if let Some(guard) = &mut state.driver_guard {
                    guard.store(
                        core,
                        address,
                        physical,
                        size as u32,
                        value & (u64::MAX >> ((8 - size) * 8)),
                    );
                }
            }
            c.write_reg(core, arm::CORE[rd], u32::from(!success))?;
        }
        return c.step_over(core, address as u32 + 4);
    }
    if word == 0xf57ff01f {
        state.exclusive[core] = None;
        return c.step_over(core, address as u32 + 4);
    }
    // MPIDR is an architectural cluster input (Unicorn standalone engines
    // each default to CPU0). CBAR is an explicit reset input, never inferred.
    if cp_read == 0x0e300f10 && c.cache_ids.is_some() {
        let rd = ((word >> 12) & 15) as usize;
        if rd == 15 {
            return Err("Unsupported CCSIDR-to-APSR read".into());
        }
        c.write_reg(core, arm::CORE[rd], c.cp(core, 0, 0, 1, 0)? as u32)?;
        return c.step_over(core, address as u32 + 4);
    }
    if cp_read == 0x0e100fb0 || cp_read == 0x0e9f0f10 {
        if (word >> 12) & 15 == 15 {
            return Err("Unsupported MRC-to-APSR identity read".into());
        }
        let value = if cp_read == 0x0e100fb0 {
            0x80000000 | core as u32
        } else {
            state.cbar_reads += 1;
            if c.cbar.is_none() {
                state.discovery.record_kind(
                    core,
                    0,
                    address as u32,
                    4,
                    false,
                    0,
                    "unknown-cbar-reset-input",
                );
            }
            c.cbar.unwrap_or(0)
        };
        c.write_reg(core, arm::CORE[((word >> 12) & 15) as usize], value)?;
        return c.step_over(core, address as u32 + 4);
    }
    if hint == 0x0320f003 || hint == 0x0320f002 {
        // architectural WFI/WFE
        if hint == 0x0320f002 && state.events[core] {
            state.events[core] = false;
        } else {
            state.waiting[core] = if hint == 0x0320f003 { 1 } else { 2 };
        }
        return c.redirect(core, address as u32 + 4);
    }
    if hint == 0x0320f004 {
        state.events.fill(true);
        for wait in &mut state.waiting {
            if *wait == 2 {
                *wait = 0;
            }
        }
        return c.redirect(core, address as u32 + 4);
    }
    Ok(())
}
unsafe fn invalid(kind: Fault, address: u64, size: i32, _value: i64, data: *mut c_void) -> bool {
    let hook = unsafe { &*data.cast::<Hook>() };
    let c = unsafe { &*hook.cluster };
    let discovery = unsafe { (*c.state.get()).discovery.enabled };
    let owned = unsafe {
        (*c.state.get())
            .devices
            .as_ref()
            .is_some_and(|d| d.owns(address, size.max(0) as u32))
    };
    if (discovery || owned)
        && matches!(kind, Fault::ReadUnmapped | Fault::WriteUnmapped)
        && address >= c.ram.len() as u64
        && size > 0
        && address + size as u64 <= 1 << 32
    {
        let pc = c.reg(hook.core, arm::PC).unwrap_or(0);
        if !owned {
            unsafe {
                (*c.state.get()).discovery.record_kind(
                    hook.core,
                    address,
                    pc,
                    size as u32,
                    kind == Fault::WriteUnmapped,
                    _value as u64,
                    "unmapped",
                );
            }
        }
        if let Err(error) = c
            .map_page(address)
            .and_then(|_| c.map_page(address + size as u64 - 1))
        {
            c.fail(hook.core, error);
            return false;
        }
        return true;
    }
    c.fail(
        hook.core,
        format!("Unmapped/protected ARMv7-A access kind={kind:?} address={address:#x} size={size}"),
    );
    false
}
unsafe fn interrupt(exception: arm::Exception, data: *mut c_void) {
    let hook = unsafe { &*data.cast::<Hook>() };
    let c = unsafe { &*hook.cluster };
    let result = match exception {
        arm::Exception::PrefetchAbort => c.deliver_abort(hook.core, true),
        arm::Exception::DataAbort => c.deliver_abort(hook.core, false),
        _ => Err(format!("ARMv7-A synchronous exception {exception:?}")),
    };
    if let Err(error) = result {
        c.fail(hook.core, error);
    }
}
unsafe fn store(_kind: Access, address: u64, size: i32, _value: i64, data: *mut c_void) {
    let hook = unsafe { &*data.cast::<Hook>() };
    let c = unsafe { &*hook.cluster };
    let state = unsafe { &mut *c.state.get() };
    if let Some(guard) = &mut state.driver_guard {
        guard.store(
            hook.core,
            c.reg(hook.core, arm::PC).unwrap_or(0) as u64,
            address,
            size as u32,
            _value as u64,
        );
    }
    for monitor in &mut state.exclusive {
        if monitor.is_some_and(|(p, n)| p < address + size as u64 && address < p + n as u64) {
            *monitor = None;
        }
    }
}
unsafe fn mmio_read(offset: u64, size: u32, data: *mut c_void) -> u64 {
    access(offset, size, false, 0, data)
}
unsafe fn mmio_write(offset: u64, size: u32, value: u64, data: *mut c_void) {
    access(offset, size, true, value, data);
}
fn access(offset: u64, size: u32, write: bool, value: u64, data: *mut c_void) -> u64 {
    let hook = unsafe { &*data.cast::<MmioHook>() };
    let c = unsafe { &*hook.cluster };
    let address = hook.base + offset;
    if !(1..=8).contains(&size) {
        c.fail(hook.core, "Unsupported MMIO width".into());
        return 0;
    }
    let counted = unsafe { (*c.state.get()).counted_core } == Some(hook.core);
    if counted && let Err(error) = c.sync_counted(hook.core) {
        c.fail(hook.core, error);
        return 0;
    }
    let result = (|| -> Result<u64, String> {
        let pc = c.reg(hook.core, arm::PC)?;
        if let Some(guard) = unsafe { &mut (*c.state.get()).driver_guard } {
            guard.mmio_access(hook.core, address, size, write)?;
        }
        let device = c.device_access(hook.core, address, size, write, value)?;
        if let Some((reply, false)) = device {
            return Ok(reply);
        }
        let state = unsafe { &mut *c.state.get() };
        let mut owned = device.is_some();
        if let (Some(base), Some(timer)) = (c.timer_base, &mut state.timer) {
            let relative = address.wrapping_sub(base as u64);
            if relative < timer.window() {
                owned = true;
                match timer.access(state.ticks, relative as u32, size, write, value as u32) {
                    Ok(value) => return Ok(value as u64),
                    Err(error) if !state.discovery.enabled => return Err(error),
                    _ => (),
                }
            }
        }
        if let Some(base) = c.cbar {
            let relative = address.wrapping_sub(base as u64);
            if (0x1000..0x3000).contains(&relative) {
                owned = true;
                match state.gic.access(
                    hook.core,
                    relative < 0x2000,
                    (relative & 0xfff) as u32,
                    size,
                    write,
                    value as u32,
                ) {
                    Ok(value) => return Ok(value as u64),
                    Err(error) if !state.discovery.enabled => return Err(error),
                    _ => (),
                }
            }
        }
        if !state.discovery.enabled {
            return Err(format!("Unsupported MMIO {address:#x} at {pc:#x}"));
        }
        state.discovery.fallback_owner(address, size, !owned);
        let mut response = state.discovery.fallback(address, size, write, value);
        state
            .discovery
            .record(hook.core, address, pc, size, write, response);
        if write {
            state.discovery.observe_write(address, size);
        } else if !owned {
            response = state.discovery.observe_read(
                hook.core,
                pc,
                address,
                size,
                response,
                state.per_core[hook.core],
            );
        }
        Ok(response)
    })();
    let result = result.and_then(|response| {
        if counted {
            c.after_access(hook.core)?;
        }
        Ok(response)
    });
    if let Ok(response) = result {
        c.trace_mmio(hook.core, address, size, write, if write { value } else { response });
    }
    match result {
        Ok(response) => response,
        Err(error) => {
            c.fail(hook.core, error);
            0
        }
    }
}

pub struct Session {
    inner: Box<Cluster>,
    next_core: Cell<usize>,
    quiescent: Cell<bool>,
}
pub type InstructionObserver = Rc<dyn Fn(usize, u64) -> Result<(), String>>;
impl Session {
    pub fn set_instruction_observer(&self, observer: Option<InstructionObserver>) {
        *self.inner.pc_handler.borrow_mut() = observer;
    }

    pub fn ram_bytes(&self) -> usize {
        self.inner.ram.len()
    }
    pub fn new(
        definition: Definition,
        storage_image: Option<Vec<u8>>,
        card_files: Vec<(String, Vec<u8>)>,
    ) -> Result<Self, String> {
        let Definition {
            ram_size,
            cores,
            discovery,
            memory_regions,
            initial_registers,
            labels,
            cbar,
            timer_base,
            timer_layout,
            timer_routes,
            storage_jedec,
            storage_dma_routes,
            spi_inputs,
            control_requests,
            storage_geometry,
            storage_irq,
            storage_dma_audio,
            storage_dma_lens,
            xpp_storage,
            xpp_serial,
            xpp_sub_cpu,
            xpp_battery_gauge,
            xpp_idle_serial,
            xpp_lens,
            xpp_shutter,
            xpp_key_lines,
            xpp_display_frames,
            xpp_adc,
            xpp_display_outputs,
            xpp_imaging_control,
            xpp_reset_gates,
            xpp_sd,
            sd_hosts,
            xpp_i2c,
            fi2c,
            jpeg_encoder,
            jpeg_decoder,
            timers16,
            pl011,
            job_ack,
            mali_v61,
            xpp_dram,
            dram_alias,
            xpp_power,
            xpp_usb,
            xpp_frame_sync,
            xpp_hdmi,
            xpp_statistics,
            xpp_statistics_scene,
            xpp_statistics_buffers,
            xpp_live_view_image,
            xpp_face,
            xpp_register_blocks,
            xpp_clocks,
            cache_sizes,
            io_group_base,
            decompressor,
            lcdc,
            dmp,
            adc,
            gpio,
            gpio_base,
            gpio_irqs,
            gpio_irq_levels,
            temperature,
            power_spi,
            gauge_spi,
            imaging_start_pulse,
            imaging_statistics_window,
            imaging_dma,
            imaging_input_events,
            imaging_still_readout_ticks,
            imaging_dma_whole_frames,
            imaging_raw_scene,
            imaging_dma_line_pitch,
            imaging_tg_cadence,
            imaging_tg_restart_delay,
            imaging_sensor_exposure,
            imaging_statistics_outputs,
            imaging_standby_timing,
            checksum_power_spi,
            imaging,
            movdc,
            movdc_scene,
            movdc_yuv420,
            sd_clocks,
            instructions_per_tick,
            per_core_time,
            usb,
            usb_phy,
            xpp_boot,
            pmu_cycles_per_tick,
            xpp_serial_replies,
        } = definition;
        let instruction_rate = fujimod_emulator_cpu::timeline::Ratio::new(
            instructions_per_tick.0,
            instructions_per_tick.1,
        )?;
        if instructions_per_tick.0 < instructions_per_tick.1 {
            return Err("A7 needs at least one instruction per guest tick".to_owned());
        }
        if !(1..=4).contains(&cores)
            || pmu_cycles_per_tick == Some(0)
            || ram_size < 0x10000
            || ram_size & 4095 != 0
            || ram_size > 0x80000000
        {
            return Err("ARMv7-A needs 1..4 cores and page aligned physical RAM".to_owned());
        }
        if !discovery && cbar.is_none() {
            return Err("Strict ARMv7-A requires an explicit CBAR reset input".to_owned());
        }
        if !discovery
            && power_spi
                .as_ref()
                .is_some_and(|(_, _, _, _, _, inputs, _)| inputs.contains_key(&24))
        {
            return Err("Provisional power status input requires discovery mode".to_owned());
        }
        // Cortex-A7 TRM DDI0464F table4-47: L1D four ways/64-byte
        // lines, L1I two ways/32-byte lines, L2 eight ways/64-byte lines.
        // Capacities are explicit processor implementation inputs.
        let cache_ids = cache_sizes
            .map(|(data, instruction, l2)| {
                if ![8192, 16384, 32768, 65536].contains(&data)
                    || ![8192, 16384, 32768, 65536].contains(&instruction)
                    || ![131072, 262144, 524288, 1048576].contains(&l2)
                {
                    return Err("Invalid Cortex-A7 cache capacities".to_owned());
                }
                Ok([
                    0x7000001a | ((data / 256 - 1) << 13),
                    0x20000009 | ((instruction / 64 - 1) << 13),
                    0x7000003a | ((l2 / 512 - 1) << 13),
                ])
            })
            .transpose()?;
        if cbar.is_some_and(|base| base & 0x7fff != 0 || u64::from(base) < ram_size as u64)
            || timer_base.is_some_and(|base| base & 3 != 0 || u64::from(base) < ram_size as u64)
            || timer_base.is_some() != timer_layout.is_some()
            || timer_routes
                .iter()
                .any(|&(_, irq)| !(32..512).contains(&irq))
        {
            return Err("Invalid ARMv7-A peripheral reset inputs".to_owned());
        }
        let ram = Backing::zeros(ram_size);
        let memory = memory::allocate(memory_regions, ram_size)?;
        let low_alias = xpp_boot
            .as_ref()
            .and_then(|boot| boot.low_alias)
            .map(|base| {
                memory
                    .iter()
                    .position(|window| window.base == base && window.data.len() < ram_size)
                    .ok_or_else(|| "Low-memory alias names no smaller memory window".to_owned())
            })
            .transpose()?;
        if let Some(alias) = dram_alias
            && (alias % 4096 != 0
                || alias < ram_size as u64
                || !alias
                    .checked_add(ram_size as u64)
                    .is_some_and(|end| end <= 1 << 32)
                || memory.iter().any(|window| {
                    alias < window.base + window.data.len() as u64
                        && window.base < alias + ram_size as u64
                }))
        {
            return Err("Invalid or overlapping ARM32 DRAM alias".into());
        }
        if !initial_registers.is_empty() && initial_registers.len() != cores {
            return Err("Initial ARM32 register sets must name every processor".into());
        }
        let translation_cache_size_bytes =
            fujimod_emulator::board::factory::translation_cache_size_bytes(cores)?;
        let library = Library::linked()?;
        let counted = counted::requested();
        let unsupported = [
            (discovery, "discovery"),
            (low_alias.is_some(), "the low boot alias"),
            (pmu_cycles_per_tick.is_some(), "the PMU profile"),
        ]
        .into_iter()
        .find_map(|(present, name)| present.then_some(name));
        if counted && let Some(name) = unsupported {
            eprintln!("FUJIMOD_XP4_COUNTED ignored: counted execution does not support {name}");
        }
        let counted = counted && unsupported.is_none();
        let mut translation_logs = Vec::new();
        let cpus = (0..cores)
            .map(|_| -> Result<Box<dyn Cpu>, String> {
                let raw = library.open(cpu::Model::CortexA7, translation_cache_size_bytes)?;
                if !counted {
                    return Ok(Box::new(raw));
                }
                let core = fujimod_emulator_cpu_unicorn::aprofile::Core::new(
                    raw,
                    fujimod_emulator_cpu_unicorn::memory::Memory::default(),
                )?;
                core.set_owner_exclusives(true)?;
                core.set_owner_interrupts(true)?;
                translation_logs.push(core.translation_log());
                Ok(Box::new(core))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut ledger = Discovery::default();
        ledger.enabled = discovery;
        let has_xpp_storage = xpp_storage.is_some();
        let lens_link = match &xpp_lens {
            Some(lens) if xpp_storage.is_some() && xpp_reset_gates.is_some() => {
                Some(fujimod_emulator_xpp::lens::LensLink::new(lens.port)?)
            }
            Some(_) => {
                return Err(
                    "The X-Processor Pro lens needs its storage PL330 and reset gates".into(),
                );
            }
            None => None,
        };
        if storage_jedec.is_some() && xpp_storage.is_some() {
            return Err("XP4 and X-Processor Pro storage cannot be selected together".to_owned());
        }
        let lens_motion = storage_dma_lens.as_ref().and_then(|lens| lens.motion_end);
        let mut devices = if let Some(jedec) = storage_jedec {
            Some(devices::Devices::storage(
                jedec,
                storage_dma_routes,
                storage_image.unwrap_or_default(),
                storage_geometry
                    .ok_or_else(|| "NOR requires explicit geometry and timing".to_owned())?,
                storage_irq
                    .ok_or_else(|| "Serial flash requires an explicit IRQ route".to_owned())?,
                storage_dma_audio,
                storage_dma_lens,
            )?)
        } else if let Some(storage) = xpp_storage {
            if !storage_dma_routes.is_empty()
                || storage_geometry.is_some()
                || storage_irq.is_some()
                || storage_dma_audio.is_some()
                || storage_dma_lens.is_some()
            {
                return Err("XP4 storage routes require the XP4 controller".to_owned());
            }
            let image = storage_image
                .ok_or_else(|| "X-Processor Pro flash requires explicit contents".to_owned())?;
            let mut devices = devices::Devices::empty();
            let windows = lens_link
                .iter()
                .map(|link| {
                    std::sync::Arc::new(link.clone())
                        as std::sync::Arc<dyn fujimod_emulator_ip_arm::pl330::Source>
                })
                .collect();
            devices.xpp_storage(storage, image, windows)?;
            Some(devices)
        } else {
            if !storage_dma_routes.is_empty()
                || storage_image.is_some()
                || storage_geometry.is_some()
                || storage_irq.is_some()
                || storage_dma_audio.is_some()
                || storage_dma_lens.is_some()
            {
                return Err(
                    "XP4 storage data/routes require explicit controller selection".to_owned(),
                );
            }
            None
        };
        let boot_wiring = xpp_boot.as_ref().map(|b| b.options.clone());
        if let Some(wiring) = &boot_wiring {
            if xpp_reset_gates != Some(wiring.selector_base)
                || xpp_power != Some(wiring.clock_base)
                || xpp_dram != Some((wiring.ddr_base, wiring.refresh_level))
            {
                return Err(
                    "Cold boot requires matching reset, power and DRAM declarations".into(),
                );
            }
        }
        let boot_lines = match xpp_boot {
            Some(boot) => Some(
                devices
                    .get_or_insert_with(devices::Devices::empty)
                    .xpp_boot(boot.options, boot.card_strap)?,
            ),
            None => None,
        };
        if xpp_serial_replies
            .iter()
            .any(|(base, _, _)| !xpp_serial.contains(base))
        {
            return Err("Serial reply names no X-Processor Pro serial port".to_owned());
        }
        let mut sub_cpu_clock = None;
        let mut sub_cpu = match xpp_sub_cpu {
            Some(peer) => {
                if !xpp_serial.contains(&peer.port) {
                    return Err("X-Processor Pro sub-CPU needs a declared serial port".to_owned());
                }
                let mut registers = vec![0; fujimod_emulator_xpp::serial::PEER_REGISTERS];
                for (register, value) in peer.registers {
                    let slot = registers
                        .get_mut(usize::from(register))
                        .ok_or("X-Processor Pro sub-CPU register out of range")?;
                    *slot = value;
                }
                sub_cpu_clock = peer.clock;
                Some((peer.port, registers))
            }
            None => None,
        };
        let mut battery_gauge = match xpp_battery_gauge {
            Some(peer) if xpp_serial.contains(&peer.port) => {
                Some((peer.port, peer.registers.into_iter().collect()))
            }
            Some(_) => {
                return Err("X-Processor Pro battery gauge needs a declared serial port".to_owned());
            }
            None => None,
        };
        let peers = [
            sub_cpu.as_ref().map(|(port, _)| *port),
            battery_gauge.as_ref().map(|(port, _)| *port),
            xpp_lens.as_ref().map(|lens| lens.port),
        ];
        if xpp_idle_serial
            .iter()
            .any(|port| !xpp_serial.contains(port) || peers.contains(&Some(*port)))
        {
            return Err("X-Processor Pro idle serial port needs a free declared port".to_owned());
        }
        if let Some(adc) = xpp_adc {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_adc(adc.base, &adc.samples, adc.conversion_ticks)?;
        }
        if let Some((base, lock_ticks, running)) = xpp_clocks {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_clocks(base, lock_ticks, running)?;
        }
        if let Some(base) = xpp_imaging_control {
            let hole = xpp_frame_sync.as_ref().map(|sync| sync.interrupts);
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_imaging_control(base, hole)?;
        }
        if let Some(sync) = xpp_frame_sync {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_frame_sync(sync)?;
        }
        let (mut external_lines, boot_resets) = match boot_lines {
            Some((resets, external)) => (Some(external), Some(resets)),
            None => (None, None),
        };
        let reset_gates = if boot_wiring.is_some() {
            boot_resets
        } else {
            match xpp_reset_gates {
                Some(base) => Some(
                    devices
                        .get_or_insert_with(devices::Devices::empty)
                        .xpp_reset_gates(base)
                        .map(|(resets, lines)| {
                            external_lines = Some(lines);
                            resets
                        })?,
                ),
                None => None,
            }
        };
        let has_xpp_sd = xpp_sd.is_some();
        if let Some(sd) = xpp_sd {
            let reset = match (sd.reset_channel, &reset_gates) {
                (Some(channel), Some(lines)) => Some(
                    lines
                        .get(channel)
                        .cloned()
                        .ok_or("Invalid X-Processor Pro SD reset channel")?,
                ),
                (Some(_), None) => {
                    return Err("X-Processor Pro SD reset line without reset gates".into());
                }
                (None, _) => None,
            };
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_sd(sd, reset, &card_files)?;
        } else if !card_files.is_empty() && sd_hosts.is_empty() {
            return Err("SD card files require an SD host".to_owned());
        }
        for (index, host) in sd_hosts.into_iter().enumerate() {
            let files: &[(String, Vec<u8>)] = if index == 0 && !has_xpp_sd { &card_files } else { &[] };
            devices
                .get_or_insert_with(devices::Devices::empty)
                .sd_host(host, files)?;
        }
        if let Some((unit, ..)) = xpp_live_view_image
            && !xpp_register_blocks
                .iter()
                .any(|b| b.base == unit && b.frames.is_some())
        {
            return Err("X-Processor Pro live-view image needs a unit with frames".into());
        }
        // Units whose words a RECC firmware color reads, published as written.
        let recc_units: BTreeMap<u64, fujimod_emulator_xpp::blocks::Published> =
            xpp_register_blocks
                .iter()
                .filter_map(|b| b.recc.as_ref()?.firmware_color)
                .flat_map(|c| {
                    let lut = c.lut3d.map(|l| {
                        std::iter::once(l.control).chain(l.tables()).collect::<Vec<_>>()
                    });
                    [c.tone, c.ycbcr].into_iter().chain(lut.into_iter().flatten())
                })
                .map(|base| (base, Default::default()))
                .collect();
        if recc_units
            .keys()
            .any(|base| !xpp_register_blocks.iter().any(|b| b.base == *base))
        {
            return Err("a RECC firmware color unit is not a declared register block".into());
        }
        for block in xpp_register_blocks {
            let image = xpp_live_view_image
                .filter(|(unit, ..)| *unit == block.base)
                .map(|(_, lines, ycbcr)| (lines, ycbcr));
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_register_block(block, image, &recc_units)?;
        }
        if let Some((base, irq, period)) = xpp_statistics {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_statistics(
                    base,
                    irq,
                    period,
                    xpp_statistics_scene,
                    &xpp_statistics_buffers,
                )?;
        } else if xpp_statistics_scene.is_some() || !xpp_statistics_buffers.is_empty() {
            return Err("X-Processor Pro statistics scene needs the statistics engine".into());
        }
        if let Some((base, irq, run_ticks)) = xpp_face {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_face(base, irq, run_ticks)?;
        }
        if let Some(base) = xpp_hdmi {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_hdmi(base)?;
        }
        if let Some((base, irq, command_ticks, transaction_ticks, speed)) = xpp_usb {
            devices.get_or_insert_with(devices::Devices::empty).usb(
                fujimod_emulator_ip::usb::dwc3::Config {
                    base,
                    irq,
                    command_ticks,
                    transaction_ticks,
                    speed,
                },
            )?;
        }
        if let Some(base) = xpp_power.filter(|_| boot_wiring.is_none()) {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_power(base)?;
        }
        if let Some((base, level)) = xpp_dram.filter(|_| boot_wiring.is_none()) {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_dram(base, level)?;
        }
        for options in job_ack {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .job_ack(options)?;
        }
        if let Some(options) = mali_v61 {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .mali_v61(options)?;
        }
        for (base, irq) in pl011 {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .pl011(base, irq)?;
        }
        if let Some((base, irq)) = timers16 {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .timers16(base, irq)?;
        }
        for (engine, decoder) in [(jpeg_encoder, false), (jpeg_decoder, true)] {
            if let Some((base, irq, ticks)) = engine {
                devices
                    .get_or_insert_with(devices::Devices::empty)
                    .jpeg(base, irq, ticks, decoder)?;
            }
        }
        for bus in fi2c {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .fi2c(bus)?;
        }
        for bus in xpp_i2c {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_i2c(bus)?;
        }
        for output in &xpp_display_outputs {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_display_output(output)?;
        }
        if let Some(frames) = xpp_display_frames {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_display_frames(frames.irqs, frames.period_ticks, frames.timing)?;
        }
        for base in xpp_serial {
            if xpp_lens.as_ref().is_some_and(|lens| lens.port == base) {
                continue;
            }
            use fujimod_emulator_xpp::serial::Peer;
            let peer = match (&sub_cpu, &battery_gauge) {
                (Some((port, _)), _) if *port == base => {
                    Peer::Registers(sub_cpu.take().map(|(_, r)| r).unwrap_or_default())
                }
                (_, Some((port, _))) if *port == base => {
                    Peer::Frames(battery_gauge.take().map(|(_, r)| r).unwrap_or_default())
                }
                _ if xpp_idle_serial.contains(&base) => Peer::Idle,
                _ => Peer::None,
            };
            let clock = match peer {
                Peer::Registers(_) => sub_cpu_clock.take(),
                _ => None,
            };
            let mut replies = BTreeMap::new();
            for &(_, command, reply) in xpp_serial_replies.iter().filter(|r| r.0 == base) {
                if replies.insert(command, reply).is_some() {
                    return Err("Repeated serial reply command".to_owned());
                }
            }
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_serial(base, peer, clock, replies)?;
        }
        if battery_gauge.is_some() {
            return Err("X-Processor Pro battery gauge shares its serial port".to_owned());
        }
        if let Some((inputs, sample_ticks)) = adc {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .adc(inputs, sample_ticks)?;
        }
        use fujimod_emulator_sensor_input::slvs::StillReadout;
        let still_readout = match (imaging_still_readout_ticks, imaging_dma_whole_frames) {
            (Some(_), true) => {
                return Err("Choose either a still read-out duration or whole frames".into());
            }
            (Some(ticks), false) => Some(StillReadout::Ticks(ticks)),
            (None, true) => Some(StillReadout::WholeFrames),
            (None, false) => None,
        };
        if let Some((map, timing, read_gate, ibuf)) = imaging {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .imaging(
                    map,
                    timing,
                    read_gate,
                    ibuf,
                    imaging_start_pulse,
                    imaging_statistics_window,
                    imaging_dma,
                    imaging_input_events,
                    still_readout,
                    imaging_raw_scene,
                    imaging_dma_line_pitch,
                    imaging_tg_cadence,
                    imaging_sensor_exposure,
                    imaging_statistics_outputs,
                )?;
            if imaging_standby_timing {
                devices
                    .as_mut()
                    .ok_or("XP4 imaging is not wired")?
                    .set_standby_timing()?;
            }
            if let Some(delay) = imaging_tg_restart_delay {
                devices
                    .as_mut()
                    .ok_or("XP4 imaging is not wired")?
                    .set_tg_restart_delay(delay.frames)?;
            }
        } else if still_readout.is_some()
            || imaging_raw_scene
            || imaging_dma_line_pitch
            || imaging_tg_cadence.is_some()
            || imaging_tg_restart_delay.is_some()
            || imaging_sensor_exposure.is_some()
            || imaging_statistics_outputs.is_some()
            || imaging_standby_timing
        {
            return Err("A still read-out, RAW scene, line pitch, cadence, restart delay, exposure, statistics output or standby timing option needs the imaging block".into());
        }
        if !movdc.is_empty() {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .movdc(movdc, movdc_scene, movdc_yuv420)?;
        } else if movdc_scene.is_some() || movdc_yuv420 {
            return Err("The MOVDC scene picture and 4:2:0 format need MOVDC outputs".into());
        }
        if let Some((base, irq)) = dmp {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .dmp(base, irq)?;
        }
        for (base, pixel_ticks, first, second, layout, video) in lcdc {
            devices.get_or_insert_with(devices::Devices::empty).lcdc(
                base,
                pixel_ticks,
                [first, second],
                layout.as_deref(),
                video.as_deref(),
            )?;
        }
        if let Some(inputs) = gpio {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .gpio(gpio_base.unwrap_or(0xfff41000), inputs)?;
        }
        if !gpio_irqs.is_empty() {
            devices
                .as_mut()
                .ok_or("GPIO interrupt lines require the GPIO wiring")?
                .gpio_irqs(gpio_irqs)?;
        }
        if gpio_irq_levels {
            devices
                .as_mut()
                .ok_or("GPIO interrupt levels require the GPIO wiring")?
                .gpio_irq_levels()?;
        }
        if let Some(config) = lens_motion {
            devices
                .as_mut()
                .ok_or("The lens motion end requires the GPIO wiring")?
                .lens_motion(config)?;
        }
        if let Some(keys) = xpp_key_lines {
            let lines = external_lines
                .clone()
                .ok_or("Key lines require the reset and gate controls")?;
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_key_lines(keys, lines)?;
        }
        if let (Some(lens), Some(link), Some(lines)) = (xpp_lens, &lens_link, external_lines) {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_lens(lens, link, lines)?;
        }
        if let Some(shutter) = xpp_shutter {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .xpp_shutter(shutter)?;
        }
        if let Some((value, ticks)) = temperature {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .temperature(value, ticks)?;
        }
        if let Some((base, idle, port, pin, high, inputs, rtc_ticks)) = power_spi {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .power_spi(base, idle, port, pin, high, inputs, rtc_ticks, gauge_spi)?;
        } else if gauge_spi.is_some() {
            return Err("The XP4 gauge SPI shares the power SPI channel".to_owned());
        }
        if let Some((
            base,
            idle,
            port,
            pin,
            high,
            inputs,
            disable_writes,
            rtc,
            retained_registers,
        )) = checksum_power_spi
        {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .checksum_power_spi(
                    base,
                    idle,
                    port,
                    pin,
                    high,
                    inputs,
                    disable_writes,
                    rtc,
                    retained_registers,
                )?;
        }
        for (base, miso) in spi_inputs {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .spi(base, miso)?;
        }
        if !control_requests.is_empty() {
            devices
                .get_or_insert_with(devices::Devices::empty)
                .requests(control_requests)?;
        }
        if !sd_clocks.is_empty() {
            if sd_clocks.iter().any(|(address, ticks)| {
                ![0xfffc402c, 0xfffc622c, 0xfffc802c, 0xfffca22c].contains(address)
                    || !(1..=54000000).contains(ticks)
            }) {
                return Err("Invalid XP4 SD host clock wiring".to_owned());
            }
            devices
                .get_or_insert_with(devices::Devices::empty)
                .sd_clocks(&sd_clocks)?;
        }
        if let Some((base, irq, command_ticks, transaction_ticks, speed)) = usb {
            if base != 0xffb00000
                || irq != 246
                || command_ticks > 54000000
                || transaction_ticks > 54000000
            {
                return Err("Invalid XP4 DWC3 wiring or functional latency".to_owned());
            }
            devices.get_or_insert_with(devices::Devices::empty).usb(
                fujimod_emulator_ip::usb::dwc3::Config {
                    base,
                    irq,
                    command_ticks,
                    transaction_ticks,
                    speed,
                },
            )?;
        }
        if let Some((base, initial, parallel)) = usb_phy {
            if base != 0xffb23800 {
                return Err("Invalid XP4 USB PHY base".to_owned());
            }
            devices
                .get_or_insert_with(devices::Devices::empty)
                .usb_phy(fujimod_emulator_xp4_io::usb_phy::Config {
                    base,
                    initial,
                    parallel,
                })?;
        }
        if let Some(base) = io_group_base {
            if base != 0xfff42000 {
                return Err("Invalid XP4 IO-group base".to_owned());
            }
            devices
                .get_or_insert_with(devices::Devices::empty)
                .io_groups(base);
        }
        if let Some((base, irq)) = decompressor {
            // The same engine sits at 0xfff79000 on XP4 and at 0xfff80000 on
            // the X-Processor Pro, whose storage is the flash-controller map.
            let expected = if has_xpp_storage {
                0xfff80000
            } else {
                0xfff79000
            };
            if base != expected || !(32..512).contains(&irq) {
                return Err("Invalid XP4 decompressor wiring".to_owned());
            }
            devices
                .get_or_insert_with(devices::Devices::empty)
                .decompressor(base, irq);
        }
        let timer = timer_layout
            .map(|layout| timer::Timer::new(layout, &timer_routes))
            .transpose()?;
        let label_set: BTreeSet<u32> = labels.iter().copied().collect();
        let mut observe_filter = vec![0u64; 1024];
        for &label in &labels {
            mark_observed(&mut observe_filter, u64::from(label));
        }
        let mut inner = Box::new(Cluster {
            cpus,
            ram,
            dram_alias,
            memory,
            hooks: Vec::new(),
            labels,
            counted,
            translation_logs,
            cbar,
            timer_base,
            cache_ids,
            instruction_rate: Cell::new(instruction_rate),
            per_core_time: Cell::new(per_core_time),
            through_exclusives: Cell::new(false),
            running: Cell::new(false),
            pc_handler: RefCell::new(None),
            state: UnsafeCell::new(RunState {
                instructions: 0,
                ticks: 0,
                tick_phase: 0,
                per_core: vec![0; cores],
                waiting: vec![0; cores],
                events: vec![false; cores],
                recent: (0..cores).map(|_| VecDeque::new()).collect(),
                milestones: BTreeMap::new(),
                discovery: ledger,
                failure: None,
                redirected: None,
                keep_slice: false,
                pages: BTreeMap::new(),
                cbar_reads: 0,
                gic: Gic::new(cores, 512),
                timer,
                irq_entries: vec![0; cores],
                undefined_entries: vec![0; cores],
                prefetch_aborts: vec![0; cores],
                data_aborts: vec![0; cores],
                calls: Vec::new(),
                pending_calls: vec![BTreeMap::new(); cores],
                observe_filter,
                accounted: 0,
                counted_core: None,
                return_requests: Vec::new(),
                observed: vec![label_set; cores],
                last_observed: vec![None; cores],
                fetch_mark: None,
                edges: None,
                exclusive: vec![None; cores],
                recorder: None,
                memory_watch: None,
                mmio_trace: None,
                pc_observers: vec![BTreeSet::new(); cores],
                devices,
                cache_dirty: false,
                pmu: (0..cores)
                    .map(|core| {
                        pmu_cycles_per_tick.map(|cycles_per_tick| {
                            fujimod_emulator::board::armv7a::Profile::new(
                                fujimod_emulator::board::armv7a::Config {
                                    mpidr: 0x80000000 | core as u32,
                                    configuration_base: cbar.unwrap_or(0),
                                    cycles_per_tick,
                                    // The XP4 board answers CCSIDR from its own cache geometry.
                                    cache_sizes: None,
                                },
                            )
                        })
                    })
                    .collect(),
                low_alias,
                remap_pending: false,
                remap_ready: false,
                driver_guard: None,
            }),
        });
        for core in 0..cores {
            let low = low_alias.map_or(0, |index| inner.memory[index].data.len());
            unsafe {
                inner.cpus[core].map_backing(
                    low as u64,
                    ram_size - low,
                    7,
                    NonNull::new(inner.ram.as_ptr().add(low)).unwrap(),
                )
            }?;
            if let Some(index) = low_alias {
                let window = &inner.memory[index];
                unsafe {
                    inner.cpus[core].map_backing(
                        0,
                        low,
                        window.permissions,
                        NonNull::new(window.data.as_ptr()).unwrap(),
                    )
                }?;
            }
            if let Some(alias) = inner.dram_alias {
                let backing = NonNull::new(inner.ram.as_ptr()).unwrap();
                if inner.counted {
                    // The counted engine tracks code by physical identity:
                    // the alias shows the same DRAM, whose identity is 0.
                    unsafe { inner.cpus[core].map_physical(alias, 0, ram_size, 7, backing) }?;
                } else {
                    unsafe { inner.cpus[core].map_backing(alias, ram_size, 7, backing) }?;
                }
            }
            for window in &inner.memory {
                unsafe {
                    inner.cpus[core].map_backing(
                        window.base,
                        window.data.len(),
                        window.permissions,
                        NonNull::new(window.data.as_ptr()).unwrap(),
                    )
                }?;
            }
            inner.hooks.push(Box::new(Hook {
                cluster: &*inner,
                core,
            }));
            let data = (&*inner.hooks[core] as *const Hook).cast_mut().cast();
            if inner.counted {
                // Labels are single-address observers; the counted engine
                // enters aborts itself and reports them through its counts.
                let labels = inner.labels.clone();
                for label in labels {
                    unsafe {
                        inner.cpus[core].install_hook(CpuHook::Instruction {
                            callback: counted::counted_observer,
                            data,
                            range: HookRange::Inclusive {
                                begin: u64::from(label),
                                end: u64::from(label),
                            },
                        })
                    }?;
                }
            } else {
                for hook in [
                    CpuHook::Instruction {
                        callback: code,
                        data,
                        range: HookRange::All,
                    },
                    CpuHook::Exception {
                        callback: interrupt,
                        data,
                    },
                ] {
                    unsafe { inner.cpus[core].install_hook(hook) }?;
                }
            }
            for hook in [
                CpuHook::Fault {
                    callback: invalid,
                    data,
                },
                CpuHook::Memory {
                    callback: store,
                    data,
                    read: false,
                    write: true,
                    range: HookRange::Inclusive {
                        begin: 0,
                        end: ram_size as u64 - 1,
                    },
                },
            ] {
                let id = unsafe { inner.cpus[core].install_hook(hook) }?;
                if inner.counted && matches!(hook, CpuHook::Memory { .. }) {
                    // Store observations only clear exclusive monitors: in
                    // counted runs they reach the board on watched pages
                    // only, and every armed monitor's page is watched. An
                    // unwatched hook would end a block at every store.
                    inner.cpus[core].hook_watched(id, true)?;
                }
            }
            for window in &inner.memory {
                if window.permissions & 2 != 0 {
                    let id = unsafe {
                        inner.cpus[core].install_hook(CpuHook::Memory {
                            callback: store,
                            data,
                            read: false,
                            write: true,
                            range: HookRange::Inclusive {
                                begin: window.base,
                                end: window.base + window.data.len() as u64 - 1,
                            },
                        })
                    }?;
                    if inner.counted {
                        // Monitors live in DRAM only, so these never fire.
                        inner.cpus[core].hook_watched(id, true)?;
                    }
                }
            }
            inner.write_reg(core, arm::CPSR, 0xd3)?;
            if let Some(registers) = initial_registers.get(core) {
                for &(register, value) in registers {
                    let value =
                        u32::try_from(value).map_err(|_| "ARM32 reset register overflows")?;
                    inner.cpus[core].write_register(register, &value.to_le_bytes())?;
                }
            }
        }
        if let Some(base) = cbar {
            inner.map_page(u64::from(base) + 0x1000)?;
            inner.map_page(u64::from(base) + 0x2000)?;
        }
        if let Some(base) = timer_base {
            inner.map_page(base as u64)?;
        }
        Ok(Self {
            inner,
            next_core: Cell::new(0),
            quiescent: Cell::new(false),
        })
    }

    /// Opt-in isolated routine instrumentation; never used by boot devices.
    pub fn driver_guard_begin(
        &self,
        executable: Vec<(u64, u64)>,
        readable: Vec<(u64, u64)>,
        writable: Vec<(u64, u64)>,
        return_address: u64,
        store_limit: usize,
        core: usize,
        device_policy: Option<String>,
    ) -> Result<(), String> {
        if self.inner.counted {
            return Err("This needs every instruction: run without FUJIMOD_XP4_COUNTED".to_owned());
        }
        self.inner.cpu(core)?;
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some()
            || state.discovery.enabled
            || state.failure.is_some()
            || (state.devices.is_some() && device_policy.is_none())
            || state.timer.is_some()
            || state.recorder.is_some()
            || state.memory_watch.is_some()
            || state.pc_observers.iter().any(|items| !items.is_empty())
        {
            return Err(
                "Driver guard requires a healthy strict cluster without devices or observers"
                    .to_owned(),
            );
        }
        if self.inner.cp(core, 1, 0, 0, 0)? & ((1 << 25) | 1) != 0 {
            return Err("Driver guard requires MMU-off little-endian execution".to_owned());
        }
        self.inner
            .memory_pointer(return_address, 4, 4)
            .map_err(|_| "Driver guard return sentinel requires executable physical backing")?;
        let policy: Option<driver_guard::DevicePolicy> = device_policy
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|error| format!("Invalid driver device policy: {error}"))
            })
            .transpose()?;
        let mut guard = driver_guard::Guard::new(
            core,
            executable,
            readable,
            writable,
            return_address,
            store_limit,
            std::iter::once((0, self.inner.ram.len() as u64))
                .chain(
                    self.inner
                        .memory
                        .iter()
                        .map(|window| (window.base, window.base + window.data.len() as u64)),
                )
                .collect(),
        )?;
        if let Some(policy) = policy {
            let mut windows = state
                .devices
                .as_ref()
                .map(|devices| devices.guard_windows())
                .unwrap_or_default();
            if let Some(base) = self.inner.cbar {
                windows.push((u64::from(base) + 0x1000, u64::from(base) + 0x3000));
            }
            guard = guard
                .with_mmio(policy.mmio, windows)?
                .with_model_memory(policy.model_readable, policy.model_writable)?;
        }
        state.driver_guard = Some(guard);
        Ok(())
    }
    pub fn driver_guard_end(&self) {
        unsafe {
            (*self.inner.state.get()).driver_guard = None;
        }
    }
    pub fn driver_guard_state(&self) -> Option<driver_guard::Snapshot> {
        unsafe {
            (*self.inner.state.get())
                .driver_guard
                .as_ref()
                .map(|guard| guard.snapshot())
        }
    }
    pub fn instruction_points(&self, core: usize, addresses: Vec<u64>) -> Result<(), String> {
        if self.inner.counted && !addresses.is_empty() {
            return Err("This needs every instruction: run without FUJIMOD_XP4_COUNTED".to_owned());
        }
        self.inner.cpu(core)?;
        if unsafe { (*self.inner.state.get()).driver_guard.is_some() } && !addresses.is_empty() {
            return Err("PC observers cannot change guarded routine inputs".to_owned());
        }
        if addresses.len() > 1 << 20 || addresses.iter().any(|a| a & 3 != 0 || *a > u32::MAX as u64)
        {
            return Err("Invalid ARMv7-A PC observer addresses".to_owned());
        }
        unsafe {
            let state = &mut *self.inner.state.get();
            for &address in &addresses {
                mark_observed(&mut state.observe_filter, address);
            }
            state.pc_observers[core] = addresses.into_iter().collect();
        }
        Ok(())
    }
    pub fn memory_watch_begin(&self, ranges: Vec<(u64, u64)>, limit: usize) -> Result<(), String> {
        if self.inner.running.get() {
            return Err("Memory watch requires a paused XP4 board".into());
        }
        if ranges.is_empty()
            || ranges.len() > 256
            || limit == 0
            || limit > 1 << 22
            || ranges
                .iter()
                .any(|&(lo, hi)| lo >= hi || hi > self.inner.ram.len() as u64)
        {
            return Err("Memory watch requires bounded physical RAM ranges".into());
        }
        let state = unsafe { &mut *self.inner.state.get() };
        if state.memory_watch.is_some() {
            return Err("Another memory watch is active".into());
        }
        if state.driver_guard.is_some() {
            return Err("Memory watch cannot attach during a guarded routine".into());
        }
        let mut ranges = ranges;
        ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for (start, end) in ranges {
            if let Some(last) = merged.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        let mut watch = observation::MemoryWatch {
            ranges: merged,
            limit,
            records: Vec::new(),
            violation: None,
            hooks: Vec::new(),
        };
        // Counted runs observe only the watched ranges: data accesses through
        // range hooks and instruction fetches through range observers (code
        // runs at its physical address on XP4); an all-RAM hook would end a
        // block at every access.
        let mut hooks = Vec::new();
        for core in 0..self.inner.cpus.len() {
            let data = (&*self.inner.hooks[core] as *const Hook).cast_mut().cast();
            if !self.inner.counted {
                hooks.push((core, CpuHook::Memory {
                    callback: memory_access,
                    data,
                    read: true,
                    write: true,
                    range: HookRange::Inclusive {
                        begin: 0,
                        end: self.inner.ram.len() as u64 - 1,
                    },
                }));
                continue;
            }
            for &(lo, hi) in &watch.ranges {
                let range = HookRange::Inclusive { begin: lo, end: hi - 1 };
                hooks.push((core, CpuHook::Memory {
                    callback: memory_access,
                    data,
                    read: true,
                    write: true,
                    range,
                }));
                hooks.push((core, CpuHook::Instruction {
                    callback: counted::counted_observer,
                    data,
                    range,
                }));
            }
        }
        for (core, hook) in hooks {
            let cpu = &self.inner.cpus[core];
            match unsafe { cpu.install_hook(hook) } {
                Ok(id) => watch.hooks.push((core, id)),
                Err(error) => {
                    for (installed_core, installed) in watch.hooks.drain(..) {
                        self.inner.cpus[installed_core].remove_hook(installed)?;
                    }
                    return Err(error);
                }
            }
        }
        unsafe {
            (*self.inner.state.get()).memory_watch = Some(watch);
        }
        Ok(())
    }
    pub fn memory_watch_end(&self) -> Result<(), String> {
        if self.inner.running.get() {
            return Err("Memory watch requires a paused XP4 board".into());
        }
        let Some(mut watch) = (unsafe { (*self.inner.state.get()).memory_watch.take() }) else {
            return Ok(());
        };
        for (core, hook) in watch.hooks.drain(..) {
            self.inner.cpus[core].remove_hook(hook)?;
        }
        Ok(())
    }
    pub fn memory_watch_enabled(&self) -> bool {
        unsafe { (*self.inner.state.get()).memory_watch.is_some() }
    }
    pub fn memory_watch_state(&self) -> observation::MemorySnapshot {
        unsafe { (*self.inner.state.get()).memory_watch.as_ref() }
            .map(observation::MemoryWatch::snapshot)
            .unwrap_or_else(observation::MemorySnapshot::disabled)
    }
    /// Starts logging guest MMIO writes (and reads if `reads`) whose address
    /// falls in `ranges` ([lo, hi) physical). Pure observation: replies,
    /// devices and timing are unchanged. At most `limit` accesses are kept;
    /// later ones are counted.
    pub fn mmio_trace_begin(&self, ranges: Vec<(u64, u64)>, limit: usize, reads: bool) -> Result<(), String> {
        if self.inner.running.get() {
            return Err("MMIO trace requires a paused XP4 board".into());
        }
        if ranges.is_empty()
            || ranges.len() > 256
            || limit == 0
            || limit > 1 << 22
            || ranges.iter().any(|&(lo, hi)| lo >= hi || hi > 1 << 32)
        {
            return Err("MMIO trace requires 1–256 bounded 32-bit ranges and 1–4M writes".into());
        }
        let state = unsafe { &mut *self.inner.state.get() };
        if state.mmio_trace.is_some() {
            return Err("Another MMIO trace is active".into());
        }
        state.mmio_trace = Some(MmioTrace { ranges, limit, reads, writes: Vec::new(), lost: 0 });
        Ok(())
    }
    /// Ends the MMIO trace and returns what it saw (empty if none was active).
    pub fn mmio_trace_end(&self) -> Result<MmioTraceSnapshot, String> {
        if self.inner.running.get() {
            return Err("MMIO trace requires a paused XP4 board".into());
        }
        let trace = unsafe { (*self.inner.state.get()).mmio_trace.take() };
        Ok(trace
            .map(|t| MmioTraceSnapshot { ranges: t.ranges, limit: t.limit, writes: t.writes, lost: t.lost })
            .unwrap_or_default())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn function_watch_begin(
        &self,
        entries: Vec<u64>,
        quota: u64,
        edge_limit: usize,
        state_limit: usize,
        return_sites: Option<Vec<u64>>,
        quota_per_edge: bool,
    ) -> Result<(), String> {
        if unsafe { (*self.inner.state.get()).driver_guard.is_some() } {
            return Err("Scalar recorder cannot attach during a guarded routine".to_owned());
        }
        if self.inner.counted {
            if self.inner.running.get() {
                return Err("The scalar recorder attaches between slices".to_owned());
            }
            // Recorder entries and returns become single-address observers.
            let addresses: Vec<u32> = entries
                .iter()
                .chain(return_sites.iter().flatten())
                .filter(|&&a| a <= u64::from(u32::MAX))
                .map(|&a| a as u32)
                .collect();
            for core in 0..self.inner.cpus.len() {
                self.inner.observe_addresses(core, &addresses)?;
            }
        }
        let recorder = observe::Recorder::new(
            entries,
            return_sites.unwrap_or_default(),
            quota,
            quota_per_edge,
            edge_limit,
            state_limit,
        )?;
        unsafe {
            (*self.inner.state.get()).recorder = Some(recorder);
        }
        Ok(())
    }
    pub fn function_watch_end(&self) {
        unsafe {
            (*self.inner.state.get()).recorder = None;
        }
    }
    pub fn function_watch_state(&self) -> Option<observe::Snapshot> {
        unsafe {
            (*self.inner.state.get())
                .recorder
                .as_ref()
                .map(|recorder| recorder.snapshot())
        }
    }

    pub fn cpr_read(
        &self,
        core: usize,
        crn: u32,
        crm: u32,
        opc1: u32,
        opc2: u32,
    ) -> Result<u64, String> {
        self.inner.cpu(core)?;
        self.inner.cp(core, crn, crm, opc1, opc2)
    }
    pub fn validate_read(&self, address: usize, size: usize) -> Result<(), String> {
        self.inner.memory_pointer(address as u64, size, 1)?;
        Ok(())
    }
    pub fn read(&self, address: usize, out: &mut [u8]) -> Result<(), String> {
        self.validate_read(address, out.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.inner.memory_pointer(address as u64, out.len(), 1)?,
                out.as_mut_ptr(),
                out.len(),
            );
        }
        Ok(())
    }
    pub fn write(&self, address: usize, data: &[u8]) -> Result<(), String> {
        let target = self.inner.memory_pointer(address as u64, data.len(), 0)?;
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        }
        Ok(())
    }
    /// Observational MMU walk restricted to RAM. Never read a device through
    /// the host debugger or discovery observer, including across page edges.
    pub fn read_virtual(&self, core: usize, address: u32, size: usize) -> Result<Vec<u8>, String> {
        self.inner.cpu(core)?;
        if size > self.inner.ram.len() || u64::from(address) + size as u64 > 1 << 32 {
            return Err("Invalid ARMv7-A virtual read extent".to_owned());
        }
        let mut bytes = Vec::with_capacity(size);
        while bytes.len() < size {
            let va = address + bytes.len() as u32;
            let chunk = (size - bytes.len()).min(4096 - (va as usize & 4095));
            let pa = self.inner.cpus[core].translate(u64::from(va), Access::Read)?;
            bytes.extend_from_slice(unsafe {
                std::slice::from_raw_parts(self.inner.memory_pointer(pa, chunk, 1)?, chunk)
            });
        }
        Ok(bytes)
    }
    pub fn run(&self, core: usize, budget: usize) -> Result<(), String> {
        let c = &self.inner;
        if c.running.replace(true) {
            return Err("Reentrant ARMv7-A execution".to_owned());
        }
        let _running = Running(&c.running);
        if budget == 0 && unsafe { (*c.state.get()).driver_guard.is_some() } {
            return Err("Guarded routine requires a positive instruction budget".to_owned());
        }
        if unsafe {
            (*c.state.get())
                .driver_guard
                .as_ref()
                .is_some_and(|guard| guard.stopped())
        } {
            return Ok(());
        }
        c.flush_dma()?;
        let cpu = c.cpu(core)?;
        let start = c.reg(core, arm::PC)?;
        unsafe {
            (*c.state.get()).redirected = None;
            (*c.state.get()).keep_slice = false;
        }
        if c.counted {
            self.run_counted(core, budget)?;
            if let Some(pc) = unsafe { (*c.state.get()).redirected.take() } {
                c.write_reg(core, arm::PC, pc)?;
            }
            c.due();
            c.flush_dma()?;
            let state = unsafe { &mut *c.state.get() };
            state.discovery.settle_trials(core, state.per_core[core]);
            return Ok(());
        }
        let slice_start = unsafe { (&(*c.state.get()).per_core)[core] };
        let mut begin = start;
        let mut left = budget;
        let mut exit;
        loop {
            exit = cpu.run(Run {
                begin: u64::from(begin),
                // No exit address: the instruction budget bounds the slice. An
                // exit at u64::MAX makes the backend invalidate the empty code
                // range [u64::MAX, u64::MAX], which Unicorn refuses (UC_ERR_ARG).
                until: None,
                watchdog_us: 0,
                instructions: Some(left),
            });
            // Unicorn reports disabled VFP as an invalid instruction rather than
            // delivering ARM's undefined exception. Let the guest's vector enable
            // and restore its FP context; never enable it from the host.
            if matches!(&exit, Err(ExecutionError::InvalidInstruction(_)))
                && unsafe { (*c.state.get()).failure.is_none() }
            {
                match c.disabled_vfp_exception(core) {
                    Ok(true) => exit = Ok(Exit::Stopped),
                    Ok(false) => (),
                    Err(error) => c.fail(core, error),
                }
            }
            // Slice policy: an exclusive or identity read the board performed
            // stopped the engine; resume after it with the rest of the budget.
            let state = unsafe { &mut *c.state.get() };
            if !std::mem::take(&mut state.keep_slice)
                || state.failure.is_some()
                || !matches!(exit, Ok(Exit::Stopped))
            {
                break;
            }
            let Some(pc) = state.redirected.take() else {
                break;
            };
            c.write_reg(core, arm::PC, pc)?;
            let used = (state.per_core[core] - slice_start) as usize;
            if budget != 0 {
                if used >= budget {
                    break;
                }
                left = budget - used;
            }
            begin = pc;
        }
        if let Some(pc) = unsafe { (*c.state.get()).redirected.take() } {
            c.write_reg(core, arm::PC, pc)?;
        }
        if unsafe { (*c.state.get()).failure.is_none() } {
            let failure = match exit {
                Ok(Exit::Stopped) => None,
                Ok(other) => Some(format!("Unexpected ARMv7-A execution exit {other:?}")),
                Err(error) => Some(error.to_string()),
            };
            unsafe {
                (*c.state.get()).failure = failure;
            }
        }
        c.due();
        c.flush_dma()?;
        let state = unsafe { &mut *c.state.get() };
        state.discovery.settle_trials(core, state.per_core[core]);
        Ok(())
    }
    pub fn idle(&self) -> bool {
        let c = &self.inner;
        c.due();
        let state = unsafe { &mut *c.state.get() };
        if state.waiting.contains(&0) {
            return true;
        }
        loop {
            let state = unsafe { &mut *c.state.get() };
            let peripheral = state
                .devices
                .as_ref()
                .and_then(|d| d.next_deadline(state.ticks));
            let timer = state
                .timer
                .as_ref()
                .and_then(|t| t.next_interrupt_tick(|irq| state.gic.pulse_would_wake(irq)));
            let Some(at) = peripheral.into_iter().chain(timer).min() else {
                return false;
            };
            state.ticks = at;
            c.due();
            if unsafe { (*c.state.get()).waiting.contains(&0) } {
                return true;
            }
            if peripheral.is_none() || unsafe { (*c.state.get()).failure.is_some() } {
                return false;
            }
        }
    }
    /// Slice policy, between two slices: with `through_exclusives`, the
    /// exclusives (LDREX/STREX and their B/H/D forms), CLREX and the identity
    /// reads (MPIDR, CBAR, modelled CCSIDR) no longer end the scheduler slice;
    /// only WFI, WFE and SEV still do. Off by default.
    pub fn set_through_exclusives(&self, on: bool) -> Result<(), String> {
        let c = &self.inner;
        if c.running.get() {
            return Err("The slice policy changes only between slices".to_owned());
        }
        c.through_exclusives.set(on);
        Ok(())
    }
    /// Changes the guest time base between two slices (instructions per tick
    /// and per-core time). The fractional tick phase restarts at zero, so at
    /// most one tick is dropped at the switch.
    pub fn set_time_base(&self, instructions_per_tick: (u64, u64), per_core_time: bool) -> Result<(), String> {
        let c = &self.inner;
        if c.running.get() {
            return Err("The time base changes only between slices".to_owned());
        }
        if instructions_per_tick.0 < instructions_per_tick.1 {
            return Err("A7 needs at least one instruction per guest tick".to_owned());
        }
        let rate = fujimod_emulator_cpu::timeline::Ratio::new(instructions_per_tick.0, instructions_per_tick.1)?;
        c.instruction_rate.set(rate);
        c.per_core_time.set(per_core_time);
        unsafe {
            (*c.state.get()).tick_phase = 0;
        }
        Ok(())
    }
    pub fn set_adc_input(&self, channel: usize, sample: u16) -> Result<(), String> {
        let c = &self.inner;
        if unsafe { (*c.state.get()).driver_guard.is_some() } {
            return Err("Guarded routine cannot change external ADC inputs".to_owned());
        }
        c.devices_due()?;
        let mut devices = unsafe { (*c.state.get()).devices.take() }
            .ok_or_else(|| "XP4 ADC is not wired".to_owned())?;
        let result = devices.set_adc_input(c, channel, sample);
        unsafe {
            (*c.state.get()).devices = Some(devices);
        }
        result
    }
    /// Imaging frame periods withheld after each later timing generator
    /// start (see `imaging_tg_restart_delay`), between two slices; 0 = none.
    /// Needs the imaging block with its cadence law.
    pub fn set_tg_restart_delay(&self, frames: u32) -> Result<(), String> {
        let c = &self.inner;
        if c.running.get() {
            return Err("The restart delay changes only between slices".to_owned());
        }
        let state = unsafe { &mut *c.state.get() };
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 imaging is not wired".to_owned())?
            .set_tg_restart_delay(frames)
    }
    /// Register-1 writes that cleared one GPIO pin (X-T3: the camera releasing
    /// the lens handshake line 15.1 before a movement); `None` without GPIO.
    pub fn gpio_releases(&self, port: usize, pin: usize) -> Option<u64> {
        let state = unsafe { &*self.inner.state.get() };
        state.devices.as_ref()?.gpio_releases(port, pin)
    }
    pub fn set_gpio_input(&self, port: usize, pin: usize, high: bool) -> Result<(), String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change external GPIO inputs".to_owned());
        }
        let devices = state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 GPIO is not wired".to_owned())?;
        drive_gpio_input(devices, &mut state.gic, port, pin, high)
    }
    /// Moves an outside-driven power-controller word mid-run (X-T3 5.11: word 0
    /// bit 10 = power switch, set = OFF).
    /// Adds outside encoder pulses to a 16-channel timer counter mid-run
    /// (X-T3 5.11: rear command dial channel 2, front channel 4). The count
    /// word depends on no device time, so no `devices_due` is needed.
    pub fn add_timer_count(&self, channel: usize, delta: i32) -> Result<u32, String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change external inputs".to_owned());
        }
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .add_timer_count(channel, delta)
    }
    pub fn set_power_input(&self, register: u8, value: u16) -> Result<(), String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change external inputs".to_owned());
        }
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .set_power_input(register, value)
    }
    /// Declares a scripted answer on a PL011 UART, mid-run.
    pub fn uart_reply(&self, base: u64, when: Vec<u8>, answer: Vec<u8>) -> Result<(), String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change external devices".to_owned());
        }
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .uart_reply(base, when, answer)
    }
    /// Every non-zero sector of the SD card in the host at `base`.
    pub fn card_sectors(&self, base: u64) -> Result<Vec<(u32, Vec<u8>)>, String> {
        let state = unsafe { &*self.inner.state.get() };
        state
            .devices
            .as_ref()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .card_sectors(base)
    }
    /// Declares the input buffer's memory read-back law, mid-run.
    pub fn input_readback(&self, event: u32, ticks: u64) -> Result<(), String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change external devices".to_owned());
        }
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .input_readback(event, ticks)
    }
    /// Attaches an RL78 flash loader to a PL011 port, mid-run.
    pub fn uart_rl78(
        &self,
        base: u64,
        signature: [u8; fujimod_emulator_xp4_io::rl78::SIGNATURE],
        echo: bool,
    ) -> Result<(), String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change external devices".to_owned());
        }
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .uart_rl78(base, signature, echo)
    }
    /// Replaces the scene the imaging models draw (statistics), mid-run.
    pub fn set_scene(&self, scene: &serde_json::Value) -> Result<usize, String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change the scene".to_owned());
        }
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .set_scene(scene)
    }
    /// Outside-driven registers of an I2C target (X-T3 5.11: the touch
    /// controller 0x55 on F_I2C channel 2), then an optional pulse of a GIC
    /// line (its interrupt, GIC 153), mid-run.
    pub fn set_i2c_registers(
        &self,
        base: u64,
        address: u16,
        registers: &std::collections::BTreeMap<u8, u16>,
        pulse: Option<u32>,
    ) -> Result<(), String> {
        let state = unsafe { &mut *self.inner.state.get() };
        if state.driver_guard.is_some() {
            return Err("Guarded routine cannot change I2C targets".to_owned());
        }
        if pulse.is_some_and(|line| !(32..512).contains(&line)) {
            return Err("i2c_registers: pulse line outside 32..512".to_owned());
        }
        state
            .devices
            .as_mut()
            .ok_or_else(|| "XP4 devices are not wired".to_owned())?
            .set_i2c_registers(base, address, registers)?;
        if let Some(line) = pulse {
            state.gic.pulse(line as usize);
        }
        Ok(())
    }
    pub fn set_usb_connected(&self, connected: bool) -> Result<(), String> {
        self.inner
            .external_devices(|d, now| d.usb_link(now, connected))
    }
    pub fn usb_bus_reset(&self) -> Result<(), String> {
        self.inner.external_devices(|d, now| d.usb_reset(now))
    }
    pub fn usb_transaction(
        &self,
        physical_endpoint: usize,
        setup: bool,
        out: Option<Vec<u8>>,
    ) -> Result<u64, String> {
        self.inner
            .external_devices(|d, now| d.usb_transaction(now, physical_endpoint, setup, out))
    }
    pub fn usb_reply_json(&self, id: u64) -> Result<Option<String>, String> {
        // Host observation must not advance deadlines, acknowledge IRQs or
        // consume a USB result. Consumption is a separate explicit action.
        let state = unsafe { &*self.inner.state.get() };
        state
            .devices
            .as_ref()
            .ok_or_else(|| "A7 devices are not wired".to_owned())?
            .usb_reply(id)
            .map(|reply| reply.map(|reply| serde_json::to_string(&reply).unwrap()))
    }
    pub fn usb_consume_reply(&self, id: u64) -> Result<(), String> {
        self.inner.external_devices(|d, _| d.usb_consume(id))
    }
    pub fn scanout_frames(&self) -> Vec<observation::Frame> {
        unsafe {
            (*self.inner.state.get())
                .devices
                .as_ref()
                .map(|devices| {
                    devices
                        .frames
                        .values()
                        .map(|frame| observation::Frame {
                            output: frame.output,
                            sequence: frame.sequence,
                            width: frame.width,
                            height: frame.height,
                            rgba: frame.rgba.clone(),
                            state_json: devices
                                .outputs
                                .get(&frame.output)
                                .map(|state| state.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
    }
    pub fn device_state_json(&self) -> BTreeMap<String, String> {
        unsafe {
            (*self.inner.state.get())
                .devices
                .as_ref()
                .map(|d| d.snapshots())
                .unwrap_or_default()
        }
    }
    pub fn interrupt_state(&self) -> Result<Vec<observation::InterruptState>, String> {
        let mut gic = unsafe { (*self.inner.state.get()).gic.clone() };
        let mut result = Vec::new();
        for core in 0..self.inner.cpus.len() {
            let mut read = |distributor, offsets: Vec<u32>| {
                offsets
                    .into_iter()
                    .map(|offset| gic.access(core, distributor, offset, 4, false, 0))
                    .collect::<Result<Vec<_>, _>>()
            };
            result.push(observation::InterruptState {
                distributor_control: read(true, vec![0])?,
                cpu_control: read(false, vec![0, 4, 8])?,
                enabled: read(true, (0x100..0x140).step_by(4).collect())?,
                pending: read(true, (0x200..0x240).step_by(4).collect())?,
                active: read(true, (0x300..0x340).step_by(4).collect())?,
            });
        }
        Ok(result)
    }
    pub fn device_records(&self) -> Vec<fujimod_emulator_devices::Record> {
        unsafe {
            (*self.inner.state.get())
                .devices
                .as_ref()
                .map(|devices| devices.log.clone())
                .unwrap_or_default()
        }
    }
    pub fn state(&self, observations: bool) -> observation::State {
        let s = unsafe { &*self.inner.state.get() };
        observation::State {
            instructions: s.instructions,
            virtual_ticks: s.ticks,
            per_core: s.per_core.clone(),
            waiting: s.waiting.iter().map(|value| *value != 0).collect(),
            irq_entries: s.irq_entries.clone(),
            undefined_entries: s.undefined_entries.clone(),
            prefetch_aborts: s.prefetch_aborts.clone(),
            data_aborts: s.data_aborts.clone(),
            next_device_tick: s
                .timer
                .as_ref()
                .and_then(|timer| timer.next_event_tick(s.ticks))
                .into_iter()
                .chain(
                    s.devices
                        .as_ref()
                        .and_then(|devices| devices.next_deadline(s.ticks)),
                )
                .min(),
            eligible_irqs: (0..self.inner.cpus.len())
                .map(|core| s.gic.next_irq(core))
                .collect(),
            failure: s.failure.clone(),
            cbar_reads: s.cbar_reads,
            gpio: s
                .devices
                .as_ref()
                .and_then(|devices| devices.gpio_registers()),
            history: observations.then(|| observation::History {
                recent: s
                    .recent
                    .iter()
                    .map(|rows| rows.iter().copied().collect())
                    .collect(),
                milestones: s
                    .milestones
                    .iter()
                    .map(|(&(core, pc), &count)| (core, pc, count))
                    .collect(),
                calls: s.calls.clone(),
            }),
        }
    }
    pub fn frontier(
        &self,
        by_count: bool,
    ) -> Vec<fujimod_emulator::board::discovery::FrontierEntry> {
        unsafe { (*self.inner.state.get()).discovery.frontier(by_count) }
    }
    pub fn discovery_shadow(&self) -> Vec<(u64, u8)> {
        unsafe { (*self.inner.state.get()).discovery.shadow_bytes() }
    }
    pub fn interventions(&self) -> Vec<fujimod_emulator::board::discovery::Intervention> {
        unsafe { (*self.inner.state.get()).discovery.interventions.clone() }
    }
}

/// The first failure of a run is its cause; later ones (a stopped core's
/// follow-up exit, a device left without deadline progress) are effects.
fn keep_first_failure(failure: &mut Option<String>, message: String) {
    if failure.is_none() {
        *failure = Some(message);
    }
}

/// Moves one external GPIO input and drives its wired interrupt lines:
/// an edge pulse on the active transition, or with `gpio_irq_levels` the
/// lines' input levels, the GIC then applying each line's own trigger
/// (a level line follows the pin, an edge line pends on the rising level).
fn drive_gpio_input(
    devices: &mut devices::Devices,
    gic: &mut Gic,
    port: usize,
    pin: usize,
    high: bool,
) -> Result<(), String> {
    let levels = devices.gpio_line_levels(port, pin, high);
    let edge = devices.set_gpio_input(port, pin, high)?;
    match levels {
        Some(levels) => {
            for (line, level) in levels {
                gic.set_line(line as usize, level);
            }
        }
        None => {
            if let Some(line) = edge {
                gic.pulse(line as usize);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod gpio_level_tests {
    use super::{devices::Devices, drive_gpio_input};
    use fujimod_emulator_ip_arm::gicv2::Gic;

    const LINE: u32 = 166;

    fn board(levels: bool) -> (Devices, Gic) {
        let mut devices = Devices::empty();
        devices.gpio(0x1000_0000, [0; 16]).unwrap();
        devices.gpio_irqs(vec![(15, 1, 182, LINE)]).unwrap();
        if levels {
            devices.gpio_irq_levels().unwrap();
        }
        let mut gic = Gic::new(1, 512);
        gic.access(0, true, 0, 4, true, 1).unwrap();
        gic.access(0, false, 0, 4, true, 1).unwrap();
        gic.access(0, false, 4, 4, true, 255).unwrap();
        (devices, gic)
    }
    /// The firmware arms the line after the pin moved: level trigger
    /// (ICFGR 0), stale pending cleared, then enabled.
    fn arm_level(gic: &mut Gic) {
        let bit = 1 << (LINE % 32);
        gic.access(0, true, 0xc00 + 4 * (LINE / 16), 4, true, 0).unwrap();
        gic.access(0, true, 0x280 + 4 * (LINE / 32), 4, true, bit).unwrap();
        gic.access(0, true, 0x100 + 4 * (LINE / 32), 4, true, bit).unwrap();
    }
    fn take(gic: &mut Gic) -> Option<u32> {
        gic.next_irq(0)?;
        let id = gic.access(0, false, 12, 4, false, 0).unwrap() & 0x3ff;
        gic.access(0, false, 16, 4, true, id).unwrap();
        Some(id)
    }

    #[test]
    fn a_pin_held_before_the_level_line_is_armed_still_interrupts() {
        let (mut devices, mut gic) = board(true);
        drive_gpio_input(&mut devices, &mut gic, 15, 1, true).unwrap();
        arm_level(&mut gic);
        // The pin is still high: the line pends again after each EOI.
        for _ in 0..10 {
            assert_eq!(take(&mut gic), Some(LINE));
        }
        drive_gpio_input(&mut devices, &mut gic, 15, 1, false).unwrap();
        assert_eq!(take(&mut gic), None);
        drive_gpio_input(&mut devices, &mut gic, 15, 1, true).unwrap();
        assert_eq!(take(&mut gic), Some(LINE));
    }
    #[test]
    fn without_levels_the_early_edge_is_lost() {
        let (mut devices, mut gic) = board(false);
        drive_gpio_input(&mut devices, &mut gic, 15, 1, true).unwrap();
        arm_level(&mut gic);
        assert_eq!(take(&mut gic), None);
        // A later transition is one pulse, not a held level.
        drive_gpio_input(&mut devices, &mut gic, 15, 1, false).unwrap();
        drive_gpio_input(&mut devices, &mut gic, 15, 1, true).unwrap();
        assert_eq!(take(&mut gic), Some(LINE));
        assert_eq!(take(&mut gic), None);
    }
    #[test]
    fn levels_need_interrupt_wiring() {
        let mut devices = Devices::empty();
        assert!(devices.gpio_irq_levels().is_err());
    }
}

#[cfg(test)]
mod failure_tests {
    use super::keep_first_failure;

    #[test]
    fn a_later_failure_keeps_the_first_cause() {
        let mut failure = None;
        keep_first_failure(&mut failure, "X-mount packet checksum/framing is invalid".into());
        keep_first_failure(&mut failure, "ARMv7-A device made no deadline progress".into());
        assert_eq!(failure.as_deref(), Some("X-mount packet checksum/framing is invalid"));
    }
}
