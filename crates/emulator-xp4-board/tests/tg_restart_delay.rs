//! `imaging_tg_restart_delay`: a definition key and a between-slices setter
//! that both need the imaging block with its timing generator cadence law.
use fujimod_emulator_xp4_board::{Definition, Session};
use serde_json::{Value, json};

fn definition(extra: Value) -> Definition {
    let mut base = json!({
        "ram_size": 65536, "cores": 1, "discovery": false, "cbar": 0xffff0000u32,
        "memory_regions": [{"base": 0xfff00000u32, "size": 4096, "permissions": 5}],
        "initial_registers": [[[{"Arm32": {"R": 15}}, 0xfff00000u32]]]
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    serde_json::from_value(base).unwrap()
}

/// The X-T3 5.11 imaging wiring of plan V91, without the read-out DMA.
fn imaging() -> Value {
    json!([[0xfff6c000u32, 0xfff69000u32, [296, 298], [297, 297], [102, 100]],
           [900900, 450450, 5400, 8], [81, 8], null])
}

fn cadence() -> Value {
    json!({"clock_hz": 72000000, "tick_hz": 54000000})
}

#[test]
fn without_the_imaging_block_the_delay_is_refused() {
    let key = definition(json!({"imaging_tg_restart_delay": {"frames": 3}}));
    assert!(Session::new(key, None, vec![]).is_err());
    let session = Session::new(definition(json!({})), None, vec![]).unwrap();
    assert!(session.set_tg_restart_delay(3).is_err());
}

#[test]
fn without_the_cadence_law_the_delay_is_refused() {
    let key = definition(json!({"imaging": imaging(), "imaging_tg_restart_delay": {"frames": 3}}));
    assert!(Session::new(key, None, vec![]).is_err());
    let session = Session::new(definition(json!({"imaging": imaging()})), None, vec![]).unwrap();
    assert!(session.set_tg_restart_delay(3).is_err());
}

#[test]
fn with_the_cadence_law_the_key_and_the_setter_are_accepted() {
    let key = definition(json!({
        "imaging": imaging(), "imaging_tg_cadence": cadence(),
        "imaging_tg_restart_delay": {"frames": 3}
    }));
    let session = Session::new(key, None, vec![]).unwrap();
    session.set_tg_restart_delay(1).unwrap();
    session.set_tg_restart_delay(0).unwrap();
    let plain = definition(json!({"imaging": imaging(), "imaging_tg_cadence": cadence()}));
    let session = Session::new(plain, None, vec![]).unwrap();
    session.set_tg_restart_delay(3).unwrap();
    // Unknown fields are refused.
    let bad = json!({"imaging_tg_restart_delay": {"frames": 3, "ticks": 1}});
    assert!(serde_json::from_value::<fujimod_emulator_sensor_input::sensor::TgRestartDelay>(
        bad["imaging_tg_restart_delay"].clone()
    )
    .is_err());
}
