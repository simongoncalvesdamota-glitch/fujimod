//! XP4 run bench: builds a board `Session` from a JSON plan (board `Definition`,
//! NOR image and RAM preloads), runs the whole board and writes a JSON report.
//!
//! Usage: `xp4_run PLAN.json REPORT.json`
//!
//! Plan keys: `definition` (an `emulator-xp4-board` Definition), `nor` (optional
//! path to the NOR image), `preload` (list of `{address, file, offset, length}`
//! or `{address, size, fill, words: [{offset, value}]}`), `budget` (instructions)
//! and `quantum` (instructions per core slice).
//!
//! Snapshots (live, by process fork): with `branch_at`, the board runs once to
//! that instruction count (executing the actions scheduled up to it), then each
//! branch is a child process forked from that exact state. `branches` lists
//! `{name, budget, actions, report, frames_dir?, dump?}` run up to `branch_jobs`
//! at a time; `serve` names a directory the parked parent watches for such
//! branch requests (`*.json`), renaming each to `.taken` and writing `.done`
//! next to it when its child exits; a file named `stop` ends the service. The
//! parent's own report describes the state at `branch_at`. Branch children
//! render DMP draws on the CPU (`FUJIMOD_SMAPH_CPU=1`): GPU frameworks do not
//! survive `fork`.
//!
//! Exit status: 0 when the budget ran out without a failure, 2 when the board
//! stopped on a failure (the execution wall), 1 on invalid input.
use fujimod_emulator_cpu::{Register, arm};
use fujimod_emulator_xp4_board::{Definition, Session};
use serde_json::{Map, Value, json};
use std::{
    env, fs,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(status: i32) -> !;
}
const WNOHANG: i32 = 1;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: xp4_run PLAN.json REPORT.json");
        std::process::exit(1);
    }
    match run(&args[1], &args[2]) {
        Ok(true) => std::process::exit(2),
        Ok(false) => (),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

/// Slice length of a `quantum` action: {instructions} > 0.
fn quantum_of(action: &Value) -> Result<usize, String> {
    match number(action, "instructions")? {
        0 => Err("quantum: instructions must be positive".into()),
        n => usize::try_from(n).map_err(|_| "quantum: instructions overflow".into()),
    }
}

/// Withheld frame periods of an `imaging_tg_restart_delay` action: {frames}
/// >= 0 (0 turns the delay off).
fn restart_delay_of(action: &Value) -> Result<u32, String> {
    let frames = action["frames"]
        .as_u64()
        .ok_or("imaging_tg_restart_delay: missing integer `frames`")?;
    u32::try_from(frames).map_err(|_| "imaging_tg_restart_delay: frames overflow".into())
}

fn number(value: &Value, key: &str) -> Result<u64, String> {
    value[key]
        .as_u64()
        .ok_or_else(|| format!("plan: missing integer `{key}`"))
}

fn preload(session: &Session, load: &Value) -> Result<usize, String> {
    let address = number(load, "address")? as usize;
    let bytes = if let Some(file) = load["file"].as_str() {
        let data = fs::read(file).map_err(|error| format!("{file}: {error}"))?;
        let offset = load["offset"].as_u64().unwrap_or(0) as usize;
        let length = load["length"]
            .as_u64()
            .map_or(data.len().saturating_sub(offset), |length| length as usize);
        data.get(offset..offset + length)
            .ok_or_else(|| format!("{file}: range outside the file"))?
            .to_vec()
    } else {
        let mut bytes = vec![load["fill"].as_u64().unwrap_or(0) as u8; number(load, "size")? as usize];
        for word in load["words"].as_array().into_iter().flatten() {
            let offset = number(word, "offset")? as usize;
            let value = number(word, "value")? as u32;
            bytes
                .get_mut(offset..offset + 4)
                .ok_or("preload word outside its block")?
                .copy_from_slice(&value.to_le_bytes());
        }
        bytes
    };
    session.write(address, &bytes)?;
    Ok(bytes.len())
}

fn hex(value: u64) -> String {
    format!("{value:#010x}")
}

fn sorted_actions(list: &Value) -> Vec<Value> {
    let mut actions: Vec<Value> = list.as_array().cloned().unwrap_or_default();
    actions.sort_by_key(|action| action["at"].as_u64().unwrap_or(0));
    actions
}

/// Largest key queue the `key` action reads (two-byte entries).
const KEY_QUEUE_ENTRIES: u32 = 136;
/// Default and step of the wait for a press to be drained before its release.
const RELEASE_WAIT: u64 = 50_000_000;
const RELEASE_STEP: u64 = 100_000;

/// True while the undrained entries `[key, state]` hold a press of `key`.
fn press_pending(entries: &[u8], key: u8) -> bool {
    entries.chunks_exact(2).any(|entry| entry[0] == key && entry[1] == 1)
}

/// `at` moved by a `wait` shift (never below 0).
fn shifted(at: u64, shift: i128) -> u64 {
    u64::try_from((i128::from(at) + shift).max(0)).unwrap_or(u64::MAX)
}

/// FNV-1a 64 of a rectangle of an RGBA frame (rows top to bottom), or None
/// when the rectangle leaves the frame.
fn region_fnv64(rgba: &[u8], width: u32, height: u32, region: [u64; 4]) -> Option<u64> {
    let [x, y, w, h] = region;
    if w == 0 || h == 0 || x + w > u64::from(width) || y + h > u64::from(height) {
        return None;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for row in y..y + h {
        let begin = ((row * u64::from(width) + x) * 4) as usize;
        for byte in rgba.get(begin..begin + (w * 4) as usize)? {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    Some(hash)
}

/// Whether a `wait` condition holds now, with what was seen (for the report).
fn condition(session: &Session, until: &Value) -> Result<(bool, Value), String> {
    match until["kind"].as_str() {
        Some("screen") => {
            let region: Vec<u64> = until["region"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
            let region: [u64; 4] = region.try_into().map_err(|_| "wait: screen needs region [x, y, w, h]")?;
            let want = until["fnv64"].as_str().ok_or("wait: screen needs fnv64")?;
            let want = u64::from_str_radix(want.trim_start_matches("0x"), 16).map_err(|_| "wait: fnv64 is not hex")?;
            let output = until["output"].as_u64().unwrap_or(1) as usize;
            let mut seen = Value::Null;
            for frame in session.scanout_frames().iter().filter(|f| f.output == output) {
                if let Some(hash) = region_fnv64(&frame.rgba, frame.width, frame.height, region) {
                    seen = json!({"fnv64": format!("{hash:#018x}"), "sequence": frame.sequence});
                    if hash == want {
                        return Ok((true, seen));
                    }
                }
            }
            Ok((false, seen))
        }
        Some("memory") => {
            let size = until["size"].as_u64().unwrap_or(4) as usize;
            if !matches!(size, 1 | 2 | 4) {
                return Err("wait: memory size must be 1, 2 or 4".into());
            }
            let mut bytes = [0u8; 4];
            session.read(number(until, "address")? as usize, &mut bytes[..size])?;
            let word = u64::from(u32::from_le_bytes(bytes)) & until["mask"].as_u64().unwrap_or(u64::MAX);
            let equal = word == number(until, "value")?;
            Ok((equal != until["not"].as_bool().unwrap_or(false), json!(hex(word))))
        }
        Some("call") => {
            let entry = number(until, "entry")?;
            let count = until["count"].as_u64().unwrap_or(1);
            let snapshot = session.function_watch_state().ok_or("wait: call needs an active watch_calls")?;
            let hits = snapshot.hits.iter().find(|(pc, _)| *pc == entry).map_or(0, |(_, n)| *n);
            Ok((hits >= count, json!(hits)))
        }
        other => Err(format!("wait: unknown condition {other:?}")),
    }
}

/// Scheduled actions, in instruction order, then the run up to `budget`.
/// `key` appends one (key, state) entry to a guest key queue whose entry count
/// lives at `count` (u32) and whose two-byte entries start at `queue`, as the
/// firmware's own scanner does; the board is stopped between slices, so no
/// guest code runs while the entry is written. A release waits (in steps of
/// `RELEASE_STEP`, at most `release_wait` instructions) until the press of the
/// same key has been drained; past that limit it is queued anyway and marked
/// `undrained`. `frames` writes the current
/// scanout under a label. The outer error is an invalid plan; the inner result
/// is the run outcome. `carry` holds the `wait` shift and the slice quantum:
/// it starts a run where a previous one stopped (a branch where its trunk
/// stopped, as in an unbranched run) and is updated on return.
fn run_actions(
    session: &Session,
    frames_dir: Option<&str>,
    actions: &[Value],
    budget: u64,
    carry: &mut Carry,
    done_actions: &mut Vec<Value>,
) -> Result<Result<(), String>, String> {
    let mut outcome = Ok(());
    let mut quantum = carry.quantum;
    let mut motion = carry.motion;
    // Moved by `wait` anchors: every later `at`, and the budget, shift by the
    // gap between the instant an awaited event happened and its plan anchor.
    let base_budget = budget;
    let mut shift: i128 = carry.shift;
    let mut budget = shifted(budget, shift);
    for action in actions {
        let at = shifted(number(action, "at")?, shift);
        let now = session.state(false).instructions;
        if at > now {
            outcome = advance(session, (at - now).min(budget.saturating_sub(now)), quantum, &mut motion, done_actions);
        }
        if outcome.is_err() || session.state(false).failure.is_some() || at > budget {
            break;
        }
        match action["kind"].as_str() {
            Some("key") => {
                let count_address = number(action, "count")? as usize;
                let queue = number(action, "queue")? as usize;
                let entry = [number(action, "key")? as u8, number(action, "state")? as u8];
                // A body cannot release a key before its press was seen. The
                // firmware's drain handles releases before presses, so a release
                // queued behind its own undrained press would leave the key held:
                // run the board in small steps until that press is drained.
                let limit = action["release_wait"].as_u64().unwrap_or(RELEASE_WAIT);
                let mut waited = 0u64;
                let mut undrained = false;
                let count = loop {
                    let mut word = [0u8; 4];
                    session.read(count_address, &mut word)?;
                    let count = u32::from_le_bytes(word).min(KEY_QUEUE_ENTRIES);
                    if entry[1] != 0 || count == 0 {
                        break count;
                    }
                    let mut entries = vec![0u8; 2 * count as usize];
                    session.read(queue, &mut entries)?;
                    if !press_pending(&entries, entry[0]) {
                        break count;
                    }
                    if waited >= limit {
                        // The firmware did not drain the queue in time (long
                        // holds, scan task asleep): keep the old behaviour.
                        undrained = true;
                        break count;
                    }
                    outcome = advance(session, RELEASE_STEP, quantum, &mut motion, done_actions);
                    waited += RELEASE_STEP;
                    if outcome.is_err() || session.state(false).failure.is_some() {
                        break count;
                    }
                };
                if outcome.is_err() || session.state(false).failure.is_some() {
                    break;
                }
                session.write(queue + 2 * count as usize, &entry)?;
                session.write(count_address, &(count + 1).to_le_bytes())?;
                done_actions.push(json!({
                    "at": at, "kind": "key", "key": entry[0], "state": entry[1], "slot": count, "deferred": waited,
                    "undrained": undrained,
                }));
            }
            Some("frames") => {
                let label = action["label"].as_str().unwrap_or("frames");
                let directory = frames_dir.unwrap_or(".");
                fs::create_dir_all(directory).map_err(|error| format!("{directory}: {error}"))?;
                let mut files = Vec::new();
                for frame in session.scanout_frames() {
                    let path = format!("{directory}/{label}-lcd{}-{}x{}.rgba", frame.output, frame.width, frame.height);
                    fs::write(&path, &frame.rgba).map_err(|error| format!("{path}: {error}"))?;
                    files.push(json!({"rgba": path, "width": frame.width, "height": frame.height, "output": frame.output, "sequence": frame.sequence}));
                }
                done_actions.push(json!({"at": at, "kind": "frames", "label": label, "files": files}));
            }
            // Drive one external GPIO input pin (e.g. X-T3 card detect, port 10
            // pin 4, active low) between slices, as a physical event would.
            Some("gpio") => {
                let port = number(action, "port")? as usize;
                let pin = number(action, "pin")? as usize;
                let high = action["high"].as_bool().ok_or("gpio action: missing bool `high`")?;
                session.set_gpio_input(port, pin, high)?;
                done_actions.push(json!({"at": at, "kind": "gpio", "port": port, "pin": pin, "high": high}));
            }
            // Set an external ADC input (sample 0..4095), e.g. the X-T3 key ladder on channel 3.
            Some("adc") => {
                let channel = number(action, "channel")? as usize;
                let sample = number(action, "sample")?;
                if sample > 4095 {
                    return Err("adc action: sample above 4095".into());
                }
                session.set_adc_input(channel, sample as u16)?;
                done_actions.push(json!({"at": at, "kind": "adc", "channel": channel, "sample": sample}));
            }
            // Encoder pulses into a 16-channel timer used as a counter (X-T3:
            // rear command dial channel 2, front channel 4): `delta` signed.
            Some("counter") => {
                let channel = number(action, "channel")? as usize;
                let delta = action["delta"].as_i64().ok_or("counter action: missing integer `delta`")?;
                let delta = i32::try_from(delta).map_err(|_| "counter action: `delta` out of range")?;
                let count = session.add_timer_count(channel, delta)?;
                done_actions.push(json!({"at": at, "kind": "counter", "channel": channel, "delta": delta,
                                         "count": hex(u64::from(count))}));
            }
            // Replace the scene the imaging statistics draw from this instruction on.
            Some("scene") => {
                let taken = session.set_scene(&action["scene"])?;
                if taken == 0 {
                    return Err("scene action: no model draws a scene".into());
                }
                done_actions.push(json!({"at": at, "kind": "scene", "models": taken}));
            }
            // {base, address, registers: [[register, value]], pulse?}: outside-driven
            // registers of a declared I2C target on the F_I2C channel at `base`, then
            // an optional GIC line pulse (X-T3 5.11 touch controller: base
            // 0xfff3f000, address 0x55, report in byte registers 1..11, pulse 153).
            Some("i2c_registers") => {
                let base = number(action, "base")?;
                let address = u16::try_from(number(action, "address")?).map_err(|_| "i2c_registers: address")?;
                let mut registers = std::collections::BTreeMap::new();
                for pair in action["registers"].as_array().ok_or("i2c_registers: registers")? {
                    let register = pair[0].as_u64().and_then(|r| u8::try_from(r).ok());
                    let value = pair[1].as_u64().and_then(|v| u16::try_from(v).ok());
                    let (Some(register), Some(value)) = (register, value) else {
                        return Err("i2c_registers: [register, value] pairs".into());
                    };
                    registers.insert(register, value);
                }
                let pulse = match action.get("pulse") {
                    None | Some(Value::Null) => None,
                    Some(_) => Some(u32::try_from(number(action, "pulse")?).map_err(|_| "i2c_registers: pulse")?),
                };
                session.set_i2c_registers(base, address, &registers, pulse)?;
                done_actions.push(json!({"at": at, "kind": "i2c_registers", "address": address,
                    "registers": registers.len(), "pulse": pulse}));
            }
            // {register, value}: an outside-driven power-controller word, e.g.
            // the X-T3 power switch (word 0, 0x8080 = ON, 0x8480 = OFF).
            Some("power_input") => {
                let register = u8::try_from(number(action, "register")?).map_err(|_| "power_input: register")?;
                let value = u16::try_from(number(action, "value")?).map_err(|_| "power_input: value")?;
                session.set_power_input(register, value)?;
                done_actions.push(json!({"at": at, "kind": "power_input", "register": register, "value": value}));
            }
            // {base, when: [bytes], answer: [bytes]}: once the bytes a PL011 UART
            // sends end with `when`, it receives `answer` (a test hypothesis).
            Some("uart_reply") => {
                let base = number(action, "base")?;
                let bytes = |key: &str| -> Result<Vec<u8>, String> {
                    action[key]
                        .as_array()
                        .ok_or(format!("uart_reply: missing `{key}`"))?
                        .iter()
                        .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()).ok_or(format!("uart_reply: `{key}` byte")))
                        .collect()
                };
                session.uart_reply(base, bytes("when")?, bytes("answer")?)?;
                done_actions.push(json!({"at": at, "kind": "uart_reply", "base": hex(base)}));
            }
            // {base, file}: every non-zero sector of the SD card in the host at
            // `base`, as records of LBA (u32 LE) + 512 bytes, written to `file`.
            Some("card_dump") => {
                let base = number(action, "base")?;
                let file = action["file"].as_str().ok_or("card_dump: missing `file`")?;
                let sectors = session.card_sectors(base)?;
                let mut out = Vec::with_capacity(sectors.len() * 516);
                for (lba, bytes) in &sectors {
                    out.extend_from_slice(&lba.to_le_bytes());
                    out.extend_from_slice(bytes);
                }
                std::fs::write(file, &out).map_err(|e| format!("card_dump: {e}"))?;
                done_actions.push(json!({"at": at, "kind": "card_dump", "base": hex(base), "file": file, "sectors": sectors.len()}));
            }
            // {event, ticks}: the input buffer's memory read-back law (a test
            // hypothesis: which +0x0c event ends a still read-back).
            Some("input_readback") => {
                let event = number(action, "event")? as u32;
                let ticks = number(action, "ticks")?;
                session.input_readback(event, ticks)?;
                done_actions.push(json!({"at": at, "kind": "input_readback", "event": hex(u64::from(event)), "ticks": ticks}));
            }
            // {base, signature?: [22 bytes], echo?: bool}: an RL78 flash
            // loader (Protocol A) on the far end of a PL011 port.
            Some("uart_rl78") => {
                use fujimod_emulator_xp4_io::rl78::{DEFAULT_SIGNATURE, SIGNATURE};
                let base = number(action, "base")?;
                let signature = match action["signature"].as_array() {
                    None => DEFAULT_SIGNATURE,
                    Some(bytes) => {
                        let bytes = bytes
                            .iter()
                            .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()).ok_or("uart_rl78: signature byte"))
                            .collect::<Result<Vec<u8>, _>>()?;
                        <[u8; SIGNATURE]>::try_from(bytes).map_err(|_| "uart_rl78: signature is 22 bytes")?
                    }
                };
                let echo = action["echo"].as_bool().unwrap_or(false);
                session.uart_rl78(base, signature, echo)?;
                done_actions.push(json!({"at": at, "kind": "uart_rl78", "base": hex(base), "echo": echo}));
            }
            // Record each core's pc, lr, sp, r0 and cpsr at this instruction;
            // many `sample` actions make a coarse sampling profiler.
            Some("memory_watch_begin") => {
                // A time window of the memory watch, all cores running:
                // {ranges: [[lo, hi], …], limit}. Ended by `memory_watch_end`.
                let ranges = action["ranges"]
                    .as_array()
                    .ok_or("memory_watch_begin: missing `ranges`")?
                    .iter()
                    .map(|r| Ok((r[0].as_u64().ok_or("memory_watch_begin: range")?, r[1].as_u64().ok_or("memory_watch_begin: range")?)))
                    .collect::<Result<Vec<_>, String>>()?;
                session.memory_watch_begin(ranges, action["limit"].as_u64().unwrap_or(65536) as usize)?;
                done_actions.push(json!({"at": at, "kind": "memory_watch_begin"}));
            }
            Some("mmio_trace_begin") => {
                // Log guest MMIO writes (and reads if `reads`) in [lo, hi) ranges
                // until `mmio_trace_end`: {ranges: [[lo, hi], …], limit, reads}.
                // Observation only; a read logs the value the board replied.
                let ranges = action["ranges"]
                    .as_array()
                    .ok_or("mmio_trace_begin: missing `ranges`")?
                    .iter()
                    .map(|r| Ok((r[0].as_u64().ok_or("mmio_trace_begin: range")?, r[1].as_u64().ok_or("mmio_trace_begin: range")?)))
                    .collect::<Result<Vec<_>, String>>()?;
                session.mmio_trace_begin(ranges, action["limit"].as_u64().unwrap_or(65536) as usize, action["reads"].as_bool().unwrap_or(false))?;
                done_actions.push(json!({"at": at, "kind": "mmio_trace_begin"}));
            }
            Some("mmio_trace_end") => {
                let trace = session.mmio_trace_end()?;
                let writes: Vec<Value> = trace
                    .writes
                    .iter()
                    .map(|w| json!([w.instructions, w.ticks, w.core, if w.read { "r" } else { "w" }, hex(w.pc), hex(w.address), w.size, hex(w.value)]))
                    .collect();
                done_actions.push(json!({
                    "at": at, "kind": "mmio_trace_end", "lost": trace.lost, "limit": trace.limit,
                    "columns": ["instructions", "ticks", "core", "access", "pc", "address", "size", "value"],
                    "writes": writes,
                }));
            }
            Some("memory_watch_end") => {
                let snapshot = serde_json::to_value(session.memory_watch_state()).unwrap_or(Value::Null);
                session.memory_watch_end()?;
                done_actions.push(json!({"at": at, "kind": "memory_watch_end", "watch": snapshot}));
            }
            Some("call") => {
                // Runs one guest function on one core, all other cores stopped:
                // {core, function, args[≤4], sentinel (an address holding `b .`),
                //  stack?, budget?, free_lock?}. The core's registers are saved and
                // restored; IRQs stay masked during the call. With `free_lock`, the
                // board first runs until that word reads 0 (the kernel SMP lock).
                let core = number(action, "core")? as usize;
                let function = number(action, "function")?;
                let sentinel = number(action, "sentinel")?;
                let budget = action["budget"].as_u64().unwrap_or(10_000_000);
                let reg = |n: usize| Register::Arm32(arm::Register::R(n as _));
                let cpsr_reg = Register::Arm32(arm::Register::Cpsr);
                let mut waited = 0u64;
                if let Some(lock) = action["free_lock"].as_u64() {
                    loop {
                        let mut word = [0u8; 4];
                        session.read(lock as usize, &mut word)?;
                        if u32::from_le_bytes(word) == 0 || waited >= 2_000_000 {
                            break;
                        }
                        session.run_board(quantum as u64, quantum)?;
                        waited += quantum as u64;
                    }
                }
                // `watch`: {ranges, limit}: every CPU read and write in the ranges during
                // the call only (the other cores are stopped), e.g. to find what a
                // getter reads.
                if let Some(watch) = action.get("watch").filter(|w| !w.is_null()) {
                    let ranges = watch["ranges"]
                        .as_array()
                        .ok_or("call: watch.ranges")?
                        .iter()
                        .map(|r| Ok((r[0].as_u64().ok_or("call: watch range")?, r[1].as_u64().ok_or("call: watch range")?)))
                        .collect::<Result<Vec<_>, String>>()?;
                    session.memory_watch_begin(ranges, watch["limit"].as_u64().unwrap_or(65536) as usize)?;
                }
                let saved = (0..16).map(|n| session.read_register(core, reg(n))).collect::<Result<Vec<_>, _>>()?;
                let cpsr = session.read_register(core, cpsr_reg)?;
                for (n, arg) in action["args"].as_array().into_iter().flatten().take(4).enumerate() {
                    session.write_register(core, reg(n), arg.as_u64().ok_or("call: args")?)?;
                }
                if let Some(stack) = action["stack"].as_u64() {
                    session.write_register(core, reg(13), stack)?;
                }
                session.write_register(core, reg(14), sentinel)?;
                session.write_register(core, cpsr_reg, (cpsr | 0x80) & !0x20)?;
                session.write_register(core, reg(15), function)?;
                let start = session.state(false).instructions;
                let mut returned = false;
                while session.state(false).instructions - start < budget {
                    session.run(core, 4096)?;
                    if session.read_register(core, reg(15))? == sentinel {
                        returned = true;
                        break;
                    }
                }
                let result = session.read_register(core, reg(0))?;
                let used = session.state(false).instructions - start;
                let watched = if action.get("watch").is_some_and(|w| !w.is_null()) {
                    let snapshot = serde_json::to_value(session.memory_watch_state()).unwrap_or(Value::Null);
                    session.memory_watch_end()?;
                    snapshot
                } else {
                    Value::Null
                };
                session.write_register(core, cpsr_reg, cpsr)?;
                for (n, value) in saved.iter().enumerate() {
                    session.write_register(core, reg(n), *value)?;
                }
                done_actions.push(json!({
                    "at": at, "kind": "call", "core": core, "function": hex(function),
                    "returned": returned, "r0": hex(result), "instructions": used, "lock_wait": waited,
                    "watch": watched,
                }));
            }
            Some("write") => {
                // `writes`: [[address, bytes (1, 2 or 4), value], …] into guest RAM,
                // between two slices (all cores stopped).
                let mut written = Vec::new();
                for item in action["writes"].as_array().ok_or("write: missing `writes`")? {
                    let address = item[0].as_u64().ok_or("write: address")? as usize;
                    let size = item[1].as_u64().ok_or("write: size")? as usize;
                    let value = item[2].as_u64().ok_or("write: value")?;
                    if !matches!(size, 1 | 2 | 4) || address % size != 0 {
                        return Err("write: size must be 1, 2 or 4 and aligned".into());
                    }
                    session.write(address, &value.to_le_bytes()[..size])?;
                    written.push(json!([hex(address as u64), size, hex(value)]));
                }
                done_actions.push(json!({"at": at, "kind": "write", "writes": written}));
            }
            // Code entered for the first time since the previous `coverage`
            // action (counted runs): the address of every instruction the
            // translator admitted, never reported twice in one run, one hex
            // address per line in `<frames_dir>/coverage-<label>.txt`.
            Some("coverage") => {
                thread_local! {
                    static SEEN: std::cell::RefCell<std::collections::HashSet<u64>> =
                        std::cell::RefCell::new(std::collections::HashSet::new());
                }
                let label = action["label"].as_str().unwrap_or("coverage");
                let directory = frames_dir.unwrap_or(".");
                fs::create_dir_all(directory).map_err(|error| format!("{directory}: {error}"))?;
                let translated = session.take_coverage()?;
                let fresh: Vec<u64> = SEEN.with(|seen| {
                    let mut seen = seen.borrow_mut();
                    translated.iter().copied().filter(|a| seen.insert(*a)).collect()
                });
                let path = format!("{directory}/coverage-{label}.txt");
                let mut text = String::with_capacity(fresh.len() * 11);
                for address in &fresh {
                    text.push_str(&format!("{address:#x}\n"));
                }
                fs::write(&path, text).map_err(|error| format!("{path}: {error}"))?;
                done_actions.push(json!({
                    "at": at, "kind": "coverage", "label": label, "file": path,
                    "new_instructions": fresh.len(), "translated": translated.len(),
                }));
            }
            // Indirect-branch edges seen so far (counted runs), one line each:
            // `branch target entries kind` in `<frames_dir>/indirect-<label>.txt`.
            Some("indirect") => {
                let label = action["label"].as_str().unwrap_or("indirect");
                let directory = frames_dir.unwrap_or(".");
                fs::create_dir_all(directory).map_err(|error| format!("{directory}: {error}"))?;
                let edges = session.indirect_edges();
                let path = format!("{directory}/indirect-{label}.txt");
                let mut text = String::new();
                for (branch, target, entries, kind) in &edges {
                    text.push_str(&format!("{branch:#x} {target:#x} {entries} {kind}\n"));
                }
                fs::write(&path, text).map_err(|error| format!("{path}: {error}"))?;
                done_actions.push(json!({"at": at, "kind": "indirect", "label": label, "file": path, "edges": edges.len()}));
            }
            Some("time_base") => {
                // Switch the guest time base between two slices, e.g. menus at
                // [4, 1] then the capture at a calibrated rate.
                let rate = &action["instructions_per_tick"];
                let rate = (
                    rate[0].as_u64().ok_or("time_base: instructions_per_tick[0]")?,
                    rate[1].as_u64().ok_or("time_base: instructions_per_tick[1]")?,
                );
                let per_core_time = action["per_core_time"].as_bool().unwrap_or(false);
                session.set_time_base(rate, per_core_time)?;
                done_actions.push(json!({
                    "at": at, "kind": "time_base", "instructions_per_tick": [rate.0, rate.1],
                    "per_core_time": per_core_time, "ticks": session.state(false).virtual_ticks,
                }));
            }
            Some("imaging_tg_restart_delay") => {
                // Withhold the first `frames` imaging frame periods after each
                // later timing generator start (0 = off), between two slices.
                // Works without the definition key, but needs the imaging
                // block and its `imaging_tg_cadence` law.
                let frames = restart_delay_of(action)?;
                session.set_tg_restart_delay(frames)?;
                done_actions.push(json!({"at": at, "kind": "imaging_tg_restart_delay", "frames": frames}));
            }
            Some("slice_policy") => {
                // Off by default: exclusives, CLREX and identity reads keep
                // the scheduler slice; only WFI/WFE/SEV still end it.
                let on = action["through_exclusives"]
                    .as_bool()
                    .ok_or("slice_policy: through_exclusives must be a boolean")?;
                session.set_through_exclusives(on)?;
                done_actions.push(json!({"at": at, "kind": "slice_policy", "through_exclusives": on}));
            }
            Some("sample") => {
                let mut cores = Vec::new();
                for core in 0..16 {
                    let read = |r| session.read_register(core, Register::Arm32(r));
                    let Ok(pc) = read(arm::Register::R(15)) else { break };
                    let text = |r| read(r).map(hex).unwrap_or_default();
                    cores.push(json!({
                        "pc": hex(pc), "lr": text(arm::Register::R(14)), "sp": text(arm::Register::R(13)),
                        "r0": text(arm::Register::R(0)), "cpsr": text(arm::Register::Cpsr),
                    }));
                }
                let observed = session.state(false);
                done_actions.push(json!({
                    "at": at, "kind": "sample", "cores": cores, "ticks": observed.virtual_ticks,
                    "per_core": observed.per_core, "irq_entries": observed.irq_entries,
                }));
            }
            // Record guest RAM words at this instruction: `ranges` = [[address, bytes], …]
            // (word-aligned, at most 4 KiB each), kept in the report.
            Some("peek") => {
                let mut ranges = Vec::new();
                for range in action["ranges"].as_array().into_iter().flatten() {
                    let address = range[0].as_u64().ok_or("peek: range address")? as usize;
                    let size = range[1].as_u64().ok_or("peek: range size")? as usize;
                    if address % 4 != 0 || size % 4 != 0 || size > 4096 {
                        return Err("peek: ranges must be word-aligned and at most 4 KiB".into());
                    }
                    let mut bytes = vec![0u8; size];
                    session.read(address, &mut bytes)?;
                    let words: Vec<String> = bytes
                        .chunks_exact(4)
                        .map(|w| hex(u64::from(u32::from_le_bytes([w[0], w[1], w[2], w[3]]))))
                        .collect();
                    ranges.push(json!({"address": hex(address as u64), "words": words}));
                }
                done_actions.push(json!({"at": at, "kind": "peek", "ranges": ranges}));
            }
            // Start the passive call recorder (arguments and results of the
            // listed entry points) from this instruction on.
            Some("watch_calls") => {
                let list = |key: &str| -> Vec<u64> {
                    action[key].as_array().into_iter().flatten().filter_map(Value::as_u64).collect()
                };
                let entries = list("entries");
                let returns = list("returns");
                session.function_watch_begin(
                    entries.clone(),
                    action["quota"].as_u64().unwrap_or(64),
                    action["edge_limit"].as_u64().unwrap_or(1 << 16) as usize,
                    action["state_limit"].as_u64().unwrap_or(1 << 18) as usize,
                    Some(returns),
                    action["per_edge"].as_bool().unwrap_or(true),
                )?;
                done_actions.push(json!({"at": at, "kind": "watch_calls", "entries": entries.len()}));
            }
            // Instructions per core slice from here on (the plan's `quantum`
            // stays the boot's: the X-T3 loader needs 20000).
            // Lens movement end (X-T3 5.11, port 15 pin 1): from here on, each
            // time the firmware releases the pin (a register-1 write clearing it,
            // 0x107159c(0xf0001, 0) before every movement) the lens pulls the
            // input low, then lets it rise `delay` instructions later (default
            // 1,500,000): the rising edge ends the movement (job 0x6d). The pin is
            // checked every `poll` instructions (default 100,000), so the low
            // level and the delay start up to `poll` after the write. `off: true`
            // stops it. Without this action the run is unchanged.
            Some("lens_motion_end") => {
                if action["off"].as_bool().unwrap_or(false) {
                    motion = None;
                    done_actions.push(json!({"at": at, "kind": "lens_motion_end", "off": true}));
                } else {
                    if session.device_state_json().contains_key("lens-motion-end") {
                        return Err("lens_motion_end: the lens definition already drives its ready line".into());
                    }
                    let port = action["port"].as_u64().unwrap_or(15) as usize;
                    let pin = action["pin"].as_u64().unwrap_or(1) as usize;
                    let delay = action["delay"].as_u64().unwrap_or(1_500_000).max(1);
                    let poll = action["poll"].as_u64().unwrap_or(100_000).max(1);
                    let seen = session
                        .gpio_releases(port, pin)
                        .ok_or("lens_motion_end: no such GPIO pin")?;
                    motion = Some(MotionEnd { port, pin, delay, poll, seen, due: None });
                    done_actions.push(json!({
                        "at": at, "kind": "lens_motion_end", "port": port, "pin": pin,
                        "delay": delay, "poll": poll, "releases": seen,
                    }));
                }
            }
            Some("quantum") => {
                quantum = quantum_of(action)?;
                done_actions.push(json!({"at": at, "kind": "quantum", "instructions": quantum}));
            }
            // Event anchor. From this instruction the board runs in steps of `step`
            // (default 1,000,000; keep it a multiple of the quantum) until `until`
            // holds, at most `limit` instructions (default 500,000,000). With
            // `anchor` (the instruction count at which the plan expects the event),
            // every later action and the budget move by (instant met - anchor), so
            // a key script keeps its spacing after the event under any time base.
            // `until`: {kind: "screen", output?, region: [x, y, w, h], fnv64}
            // (FNV-1a 64 of the region's RGBA rows, as a hex string), {kind:
            // "memory", address, size? (1, 2 or 4), mask?, value, not?} or {kind:
            // "call", entry, count?} (an entry of an active `watch_calls`). Not met
            // in time: no shift, `met: false`; with `required` the run stops.
            Some("wait") => {
                let until = &action["until"];
                let step = action["step"].as_u64().unwrap_or(1_000_000).max(1);
                let limit = action["limit"].as_u64().unwrap_or(500_000_000);
                let start = session.state(false).instructions;
                let (mut met, mut seen) = condition(session, until)?;
                while !met {
                    let waited = session.state(false).instructions - start;
                    if waited >= limit {
                        break;
                    }
                    outcome = advance(session, step.min(limit - waited), quantum, &mut motion, done_actions);
                    if outcome.is_err() || session.state(false).failure.is_some() {
                        break;
                    }
                    (met, seen) = condition(session, until)?;
                }
                let now = session.state(false).instructions;
                if met {
                    if let Some(anchor) = action["anchor"].as_u64() {
                        shift = i128::from(now) - i128::from(anchor);
                        budget = shifted(base_budget, shift);
                    }
                }
                done_actions.push(json!({
                    "at": at, "kind": "wait", "met": met, "instructions": now, "waited": now - start,
                    "shift": shift.to_string(), "budget": budget, "seen": seen,
                }));
                if outcome.is_err() || session.state(false).failure.is_some() {
                    break;
                }
                if !met && action["required"].as_bool().unwrap_or(false) {
                    outcome = Err(format!("wait at {at}: condition not met within {limit} instructions"));
                    break;
                }
            }
            other => return Err(format!("unknown action kind {other:?}")),
        }
    }
    if outcome.is_ok() && session.state(false).failure.is_none() {
        let now = session.state(false).instructions;
        if budget > now {
            outcome = advance(session, budget - now, quantum, &mut motion, done_actions);
        }
    }
    *carry = Carry { shift, quantum, motion };
    Ok(outcome)
}

/// Run state that outlives a list of actions: the `wait` shift, the slice
/// quantum set by a `quantum` action and an armed `lens_motion_end`.
#[derive(Clone, Copy)]
struct Carry {
    shift: i128,
    quantum: usize,
    motion: Option<MotionEnd>,
}

/// An armed `lens_motion_end`: the pin, its timing, the release count already
/// answered and the instant the pending rise is due.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MotionEnd {
    port: usize,
    pin: usize,
    delay: u64,
    poll: u64,
    seen: u64,
    due: Option<u64>,
}

impl MotionEnd {
    /// One check at instruction `now` with `releases` counted so far: the
    /// level to drive now, if any. A new release lowers the pin and (re)starts
    /// the delay; a due rise raises it. A count that went back (a restored
    /// state) is taken as is.
    fn check(&mut self, now: u64, releases: u64) -> Option<bool> {
        if releases > self.seen {
            self.seen = releases;
            self.due = Some(now.saturating_add(self.delay));
            return Some(false);
        }
        self.seen = releases;
        match self.due {
            Some(due) if now >= due => {
                self.due = None;
                Some(true)
            }
            _ => None,
        }
    }
}

/// `run_board` for `limit` instructions; with an armed `lens_motion_end`, in
/// steps of at most its `poll` (cut at a due rise), driving the pin between
/// steps and logging each move. Unarmed, exactly one `run_board` call.
fn advance(
    session: &Session,
    limit: u64,
    quantum: usize,
    motion: &mut Option<MotionEnd>,
    log: &mut Vec<Value>,
) -> Result<(), String> {
    let Some(m) = motion.as_mut() else {
        return session.run_board(limit, quantum);
    };
    let start = session.state(false).instructions;
    let target = start.saturating_add(limit);
    loop {
        let now = session.state(false).instructions;
        if now >= target || session.state(false).failure.is_some() {
            return Ok(());
        }
        let mut step = (target - now).min(m.poll);
        if let Some(due) = m.due {
            step = step.min(due.saturating_sub(now).max(1));
        }
        session.run_board(step, quantum)?;
        let after = session.state(false).instructions;
        let releases = session
            .gpio_releases(m.port, m.pin)
            .ok_or("lens_motion_end: no such GPIO pin")?;
        if let Some(high) = m.check(after, releases) {
            session.set_gpio_input(m.port, m.pin, high)?;
            log.push(json!({
                "at": after, "kind": "lens_motion_end", "high": high, "releases": releases,
            }));
        }
        if after == now {
            // Quiescent board: `run_board` made no progress.
            return Ok(());
        }
    }
}

/// Writes the report (`plan` supplies `dump` and `frames_dir`) and prints the
/// one-line summary. Only reads the board. Returns whether the run stopped.
#[allow(clippy::too_many_arguments)]
fn finish(
    session: &Session,
    cores: usize,
    plan: &Value,
    loaded: &[Value],
    outcome: Result<(), String>,
    seconds: f64,
    done_actions: Vec<Value>,
    report_path: &str,
) -> Result<bool, String> {
    let state = session.state(true);
    let failure = state.failure.clone();
    let mut registers = Vec::new();
    for core in 0..cores {
        let mut row = Map::new();
        for n in 0..16u8 {
            if let Ok(value) = session.read_register(core, Register::Arm32(arm::Register::R(n))) {
                row.insert(format!("r{n}"), json!(hex(value)));
            }
        }
        if let Ok(value) = session.read_register(core, Register::Arm32(arm::Register::Cpsr)) {
            row.insert("cpsr".into(), json!(hex(value)));
        }
        registers.push(Value::Object(row));
    }
    let frontier: Vec<Value> = session
        .frontier(false)
        .into_iter()
        .map(|entry| {
            json!({
                "first": entry.first, "count": entry.count, "cpu": entry.cpu,
                "address": hex(entry.address), "pc": hex(u64::from(entry.pc)),
                "size": entry.size, "write": entry.write,
                "value": format!("{:#x}", entry.value), "kind": entry.kind,
            })
        })
        .collect();
    let interventions: Vec<Value> = session
        .interventions()
        .into_iter()
        .map(|item| {
            json!({
                "kind": item.kind, "cpu": item.cpu, "address": hex(item.address),
                "pc": hex(u64::from(item.pc)), "size": item.size,
                "before": format!("{:#x}", item.before), "value": format!("{:#x}", item.value),
                "instructions": item.instructions, "outcome": item.outcome, "evidence": item.evidence,
            })
        })
        .collect();
    let milestones: Vec<Value> = state
        .history
        .as_ref()
        .map(|history| {
            history
                .milestones
                .iter()
                .map(|(core, pc, at)| json!({"core": core, "pc": hex(u64::from(*pc)), "instructions": at}))
                .collect()
        })
        .unwrap_or_default();
    let mut dumps = Vec::new();
    for dump in plan["dump"].as_array().into_iter().flatten() {
        let address = number(dump, "address")? as usize;
        let mut bytes = vec![0u8; number(dump, "size")? as usize];
        let file = dump["file"].as_str().ok_or("dump: missing `file`")?;
        session.read(address, &mut bytes)?;
        fs::write(file, &bytes).map_err(|error| format!("{file}: {error}"))?;
        dumps.push(json!({"address": hex(address as u64), "bytes": bytes.len(), "file": file}));
    }
    let frames = session.scanout_frames();
    let mut written_frames = Vec::new();
    if let Some(directory) = plan["frames_dir"].as_str() {
        fs::create_dir_all(directory).map_err(|error| format!("{directory}: {error}"))?;
        for frame in &frames {
            let stem = format!("{directory}/frame-{}-{}", frame.output, frame.sequence);
            fs::write(format!("{stem}.rgba"), &frame.rgba).map_err(|error| format!("{stem}: {error}"))?;
            written_frames.push(json!({
                "output": frame.output, "sequence": frame.sequence,
                "width": frame.width, "height": frame.height,
                "rgba": format!("{stem}.rgba"), "state": frame.state_json,
            }));
        }
    }
    let calls = session.function_watch_state().map(|snapshot| {
        json!({
            "hits": snapshot.hits.iter().map(|(pc, n)| json!([hex(*pc), n])).collect::<Vec<_>>(),
            "edges": snapshot.edges.iter().map(|(pc, lr, n)| json!([hex(*pc), hex(*lr), n])).collect::<Vec<_>>(),
            "states": snapshot.states.iter().map(|call| json!({
                "callee": hex(call.callee), "core": call.core, "instructions": call.instructions,
                "args": call.registers[..4].iter().map(|r| hex(u64::from(*r))).collect::<Vec<_>>(),
                "lr": hex(u64::from(call.registers[14])), "sp": hex(u64::from(call.registers[13])),
                "returned": call.returned.map(|(at, r0)| json!([at, hex(u64::from(r0))])),
            })).collect::<Vec<_>>(),
            "dropped_edges": snapshot.dropped_edges, "dropped_states": snapshot.dropped_states,
        })
    });
    let report = json!({
        "calls": calls,
        "memory_watch": session.memory_watch_enabled().then(|| serde_json::to_value(session.memory_watch_state()).unwrap_or(Value::Null)),
        "outcome": match &outcome { Ok(()) => json!("ok"), Err(error) => json!(error) },
        "dumps": dumps,
        "actions": done_actions,
        "frames": written_frames,
        "failure": failure,
        "seconds": seconds,
        "instructions": state.instructions,
        "virtual_ticks": state.virtual_ticks,
        "quiescent": session.quiescent(),
        "preloaded": loaded,
        "milestones": milestones,
        "registers": registers,
        "frontier": frontier,
        "interventions": interventions,
        "scanout_frames": frames.len(),
        "interrupts": session.interrupt_state().ok(),
        "devices": session
            .device_state_json()
            .into_iter()
            .map(|(name, state)| {
                let value = serde_json::from_str::<Value>(&state).unwrap_or(Value::String(state));
                (name, value)
            })
            .collect::<Map<String, Value>>(),
        "state": state,
    });
    fs::write(report_path, serde_json::to_string_pretty(&report).unwrap())
        .map_err(|error| format!("{report_path}: {error}"))?;
    let pc = registers
        .first()
        .and_then(|row| row.get("r15"))
        .cloned()
        .unwrap_or(Value::Null);
    println!(
        "{}",
        json!({
            "instructions": report["instructions"], "seconds": seconds, "pc": pc,
            "failure": report["failure"], "outcome": report["outcome"],
            "milestones": milestones.len(), "frontier": frontier.len(),
            "interventions": interventions.len(), "quiescent": report["quiescent"],
        })
    );
    Ok(failure.is_some() || outcome.is_err())
}

fn run(plan_path: &str, report_path: &str) -> Result<bool, String> {
    let text = fs::read_to_string(plan_path).map_err(|error| format!("{plan_path}: {error}"))?;
    let plan: Value = serde_json::from_str(&text).map_err(|error| format!("{plan_path}: {error}"))?;
    let branch_at = plan["branch_at"].as_u64();
    if branch_at.is_some() && env::var_os("FUJIMOD_SMAPH_CPU").is_none() {
        // Before any device exists: no GPU context is ever created in the parent.
        unsafe { env::set_var("FUJIMOD_SMAPH_CPU", "1") };
    }
    let definition: Definition = serde_json::from_value(plan["definition"].clone())
        .map_err(|error| format!("definition: {error}"))?;
    let cores = plan["definition"]["cores"].as_u64().unwrap_or(1) as usize;
    let nor = plan["nor"]
        .as_str()
        .map(|path| fs::read(path).map_err(|error| format!("{path}: {error}")))
        .transpose()?;
    // `card_files`: [{name: "/DCIM/100_FUJI/DSCF0001.JPG", path: local file}],
    // merged into the virtual SD card(s) before boot.
    let mut card_files = Vec::new();
    for file in plan["card_files"].as_array().into_iter().flatten() {
        let name = file["name"].as_str().ok_or("card_files: missing `name`")?;
        let path = file["path"].as_str().ok_or("card_files: missing `path`")?;
        let data = fs::read(path).map_err(|error| format!("{path}: {error}"))?;
        card_files.push((name.to_owned(), data));
    }
    let session = Session::new(definition, nor, card_files)?;
    // `coverage` actions need the translation log from the very start.
    if plan["actions"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|a| a["kind"] == "coverage"))
    {
        session.enable_coverage()?;
    }
    // `indirect` actions need the edge observers from the very start.
    if plan["actions"]
        .as_array()
        .is_some_and(|actions| actions.iter().any(|a| a["kind"] == "indirect"))
    {
        session.enable_indirect_edges()?;
    }
    // `memory_watch`: {ranges: [[lo, hi], …] (physical RAM), limit}: every CPU read,
    // write or fetch inside the ranges is recorded (proves an area stays unused).
    if let Some(watch) = plan.get("memory_watch").filter(|w| !w.is_null()) {
        let ranges = watch["ranges"]
            .as_array()
            .ok_or("memory_watch: missing `ranges`")?
            .iter()
            .map(|r| Ok((r[0].as_u64().ok_or("memory_watch: range")?, r[1].as_u64().ok_or("memory_watch: range")?)))
            .collect::<Result<Vec<_>, String>>()?;
        session.memory_watch_begin(ranges, watch["limit"].as_u64().unwrap_or(4096) as usize)?;
    }
    let mut loaded = Vec::new();
    for load in plan["preload"].as_array().into_iter().flatten() {
        let bytes = preload(&session, load)?;
        loaded.push(json!({"address": hex(number(load, "address")?), "bytes": bytes}));
    }
    let budget = plan["budget"].as_u64().unwrap_or(1_000_000);
    let quantum = plan["quantum"].as_u64().unwrap_or(10_000) as usize;
    let actions = sorted_actions(&plan["actions"]);

    let started = Instant::now();
    let mut done_actions = Vec::new();
    let mut carry = Carry { shift: 0, quantum, motion: None };
    let Some(branch_at) = branch_at else {
        let outcome = run_actions(
            &session,
            plan["frames_dir"].as_str(),
            &actions,
            budget,
            &mut carry,
            &mut done_actions,
        )?;
        let seconds = started.elapsed().as_secs_f64();
        return finish(&session, cores, &plan, &loaded, outcome, seconds, done_actions, report_path);
    };

    // Trunk: the actions scheduled up to the snapshot point, then the run to it.
    let trunk: Vec<Value> = actions
        .iter()
        .filter(|action| action["at"].as_u64().unwrap_or(0) <= branch_at)
        .cloned()
        .collect();
    let outcome = run_actions(
        &session,
        plan["frames_dir"].as_str(),
        &trunk,
        branch_at,
        &mut carry,
        &mut done_actions,
    )?;
    // A branch goes on with the trunk's shift and quantum: its `at` and budget
    // are plan instants, as in an unbranched run.
    let trunk_carry = carry;
    let trunk_seconds = started.elapsed().as_secs_f64();
    if outcome.is_err() || session.state(false).failure.is_some() {
        return finish(&session, cores, &plan, &loaded, outcome, trunk_seconds, done_actions, report_path);
    }
    let branch = |request: &Value| -> i32 {
        let started = Instant::now();
        let result = (|| -> Result<bool, String> {
            let mut view = plan.clone();
            for key in ["frames_dir", "dump"] {
                view[key] = request[key].clone();
            }
            let budget = number(request, "budget")?;
            let report = request["report"].as_str().ok_or("branch: missing `report`")?;
            let actions = sorted_actions(&request["actions"]);
            if actions.iter().any(|action| action["at"].as_u64().unwrap_or(0) < branch_at) {
                return Err("branch: an action is scheduled before `branch_at`".into());
            }
            let mut done = done_actions.clone();
            let mut carry = trunk_carry;
            let outcome = run_actions(&session, view["frames_dir"].as_str(), &actions, budget, &mut carry, &mut done)?;
            // A branch's time is the trunk's plus its own, as in an unbranched run.
            let seconds = trunk_seconds + started.elapsed().as_secs_f64();
            finish(&session, cores, &view, &loaded, outcome, seconds, done, report)
        })();
        match result {
            Ok(false) => 0,
            Ok(true) => 2,
            Err(error) => {
                eprintln!("branch {}: {error}", request["name"]);
                1
            }
        }
    };
    let spawn = |request: &Value| -> Result<i32, String> {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        match unsafe { fork() } {
            -1 => Err("fork failed".into()),
            0 => {
                let code = branch(request);
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                unsafe { _exit(code) }
            }
            pid => Ok(pid),
        }
    };
    let jobs = plan["branch_jobs"].as_u64().unwrap_or(1).max(1) as usize;
    // (pid, done marker to write when it exits)
    let mut running: Vec<(i32, Option<String>)> = Vec::new();
    let mut exits = Vec::new();
    let reap = |running: &mut Vec<(i32, Option<String>)>, exits: &mut Vec<Value>, block: bool| {
        let mut index = 0;
        while index < running.len() {
            let mut status = 0;
            let pid = unsafe { waitpid(running[index].0, &mut status, if block { 0 } else { WNOHANG }) };
            if pid == running[index].0 {
                let code = if status & 0x7f == 0 { (status >> 8) & 0xff } else { 128 + (status & 0x7f) };
                if let Some(marker) = &running[index].1 {
                    let _ = fs::write(marker, format!("{code}\n"));
                }
                exits.push(json!({"pid": pid, "exit": code}));
                running.remove(index);
                if block {
                    return;
                }
            } else {
                index += 1;
            }
        }
    };
    for request in plan["branches"].as_array().into_iter().flatten() {
        while running.len() >= jobs {
            reap(&mut running, &mut exits, true);
        }
        running.push((spawn(request)?, None));
    }
    if let Some(directory) = plan["serve"].as_str() {
        fs::create_dir_all(directory).map_err(|error| format!("{directory}: {error}"))?;
        let ready = format!("{directory}/ready");
        fs::write(&ready, format!("{branch_at}\n")).map_err(|error| format!("{ready}: {error}"))?;
        println!("{}", json!({"serving": directory, "branch_at": branch_at, "trunk_seconds": trunk_seconds}));
        let _ = std::io::stdout().flush();
        let stop = Path::new(directory).join("stop");
        loop {
            reap(&mut running, &mut exits, false);
            if stop.exists() {
                break;
            }
            let mut requests: Vec<_> = fs::read_dir(directory)
                .map_err(|error| format!("{directory}: {error}"))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect();
            requests.sort();
            for path in requests {
                if running.len() >= jobs {
                    break;
                }
                let taken = path.with_extension("json.taken");
                if fs::rename(&path, &taken).is_err() {
                    continue;
                }
                let request = fs::read_to_string(&taken)
                    .map_err(|error| error.to_string())
                    .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|error| error.to_string()));
                let marker = path.with_extension("json.done").to_string_lossy().into_owned();
                match request {
                    Ok(request) => running.push((spawn(&request)?, Some(marker))),
                    Err(error) => {
                        let _ = fs::write(&marker, format!("1 {error}\n"));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    while !running.is_empty() {
        reap(&mut running, &mut exits, true);
    }
    // The parked trunk was never advanced: its report is the snapshot state.
    let stopped = finish(&session, cores, &plan, &loaded, outcome, trunk_seconds, done_actions, report_path)?;
    if !exits.is_empty() {
        println!("{}", json!({"branches": exits}));
    }
    Ok(stopped)
}

#[cfg(test)]
mod tests {
    use super::{MotionEnd, press_pending, quantum_of, restart_delay_of};
    #[test]
    fn a_lens_movement_ends_one_delay_after_each_release_of_its_line() {
        // X-T3 5.11 T44-fr-s: releases of 15.1 at 1311.81M and 1348.94M, the
        // second one never answered without this action.
        let mut m = MotionEnd { port: 15, pin: 1, delay: 1_500_000, poll: 100_000, seen: 3, due: None };
        assert_eq!(m.check(1_311_900_000, 3), None);
        assert_eq!(m.check(1_312_000_000, 4), Some(false));
        assert_eq!(m.check(1_313_400_000, 4), None);
        assert_eq!(m.check(1_313_500_000, 4), Some(true));
        assert_eq!(m.check(1_320_000_000, 4), None);
        // A release while a rise is pending restarts the delay.
        assert_eq!(m.check(1_349_000_000, 5), Some(false));
        assert_eq!(m.check(1_349_100_000, 6), Some(false));
        assert_eq!(m.check(1_350_500_000, 6), None);
        assert_eq!(m.check(1_350_600_000, 6), Some(true));
        // A count that went back (restored state) is followed, not answered.
        assert_eq!(m.check(1_360_000_000, 2), None);
        assert_eq!(m.check(1_360_100_000, 3), Some(false));
    }

    #[test]
    fn a_quantum_action_names_a_positive_slice() {
        let action = |v: serde_json::Value| serde_json::json!({"at": 1, "kind": "quantum", "instructions": v});
        assert_eq!(quantum_of(&action(serde_json::json!(1000))), Ok(1000));
        assert!(quantum_of(&action(serde_json::json!(0))).is_err());
        assert!(quantum_of(&serde_json::json!({"at": 1, "kind": "quantum"})).is_err());
    }

    #[test]
    fn a_restart_delay_action_names_a_frame_count() {
        let action = |v: serde_json::Value| {
            serde_json::json!({"at": 1, "kind": "imaging_tg_restart_delay", "frames": v})
        };
        assert_eq!(restart_delay_of(&action(serde_json::json!(3))), Ok(3));
        assert_eq!(restart_delay_of(&action(serde_json::json!(0))), Ok(0));
        assert!(restart_delay_of(&action(serde_json::json!(-1))).is_err());
        assert!(restart_delay_of(&action(serde_json::json!(1u64 << 32))).is_err());
        assert!(restart_delay_of(&serde_json::json!({"at": 1, "kind": "imaging_tg_restart_delay"})).is_err());
    }

    #[test]
    fn release_waits_for_its_own_press() {
        // LEFT (28) pressed, not drained yet: its release must wait.
        assert!(press_pending(&[28, 1], 28));
        // Another key's press does not hold the release back.
        assert!(!press_pending(&[27, 1], 28));
        // A queued release of the same key is not a press.
        assert!(!press_pending(&[28, 0], 28));
        // Drained queue.
        assert!(!press_pending(&[], 28));
        assert!(press_pending(&[27, 0, 28, 1], 28));
    }
}
