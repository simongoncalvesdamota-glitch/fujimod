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

/// Scheduled actions, in instruction order, then the run up to `budget`.
/// `key` appends one (key, state) entry to a guest key queue whose entry count
/// lives at `count` (u32) and whose two-byte entries start at `queue`, as the
/// firmware's own scanner does; the board is stopped between slices, so no
/// guest code runs while the entry is written. `frames` writes the current
/// scanout under a label. The outer error is an invalid plan; the inner result
/// is the run outcome.
fn run_actions(
    session: &Session,
    frames_dir: Option<&str>,
    actions: &[Value],
    budget: u64,
    quantum: usize,
    done_actions: &mut Vec<Value>,
) -> Result<Result<(), String>, String> {
    let mut outcome = Ok(());
    for action in actions {
        let at = number(action, "at")?;
        let now = session.state(false).instructions;
        if at > now {
            outcome = session.run_board((at - now).min(budget.saturating_sub(now)), quantum);
        }
        if outcome.is_err() || session.state(false).failure.is_some() || at > budget {
            break;
        }
        match action["kind"].as_str() {
            Some("key") => {
                let count_address = number(action, "count")? as usize;
                let queue = number(action, "queue")? as usize;
                let mut word = [0u8; 4];
                session.read(count_address, &mut word)?;
                let count = u32::from_le_bytes(word);
                let entry = [number(action, "key")? as u8, number(action, "state")? as u8];
                session.write(queue + 2 * count as usize, &entry)?;
                session.write(count_address, &(count + 1).to_le_bytes())?;
                done_actions.push(json!({"at": at, "kind": "key", "key": entry[0], "state": entry[1], "slot": count}));
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
            other => return Err(format!("unknown action kind {other:?}")),
        }
    }
    if outcome.is_ok() && session.state(false).failure.is_none() {
        let now = session.state(false).instructions;
        if budget > now {
            outcome = session.run_board(budget - now, quantum);
        }
    }
    Ok(outcome)
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
    let report = json!({
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
    let session = Session::new(definition, nor, vec![])?;
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
    let Some(branch_at) = branch_at else {
        let outcome = run_actions(
            &session,
            plan["frames_dir"].as_str(),
            &actions,
            budget,
            quantum,
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
        quantum,
        &mut done_actions,
    )?;
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
            let outcome = run_actions(&session, view["frames_dir"].as_str(), &actions, budget, quantum, &mut done)?;
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
