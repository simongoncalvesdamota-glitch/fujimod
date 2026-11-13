//! `imaging_standby_timing`: an opt-in definition key that needs the imaging
//! block; off by default.
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

#[test]
fn the_key_is_off_by_default_and_needs_the_imaging_block() {
    assert!(!definition(json!({})).imaging_standby_timing);
    let alone = definition(json!({"imaging_standby_timing": true}));
    assert!(Session::new(alone, None, vec![]).is_err());
    let on = definition(json!({"imaging": imaging(), "imaging_standby_timing": true}));
    assert!(Session::new(on, None, vec![]).is_ok());
    let off = definition(json!({"imaging": imaging(), "imaging_standby_timing": false}));
    assert!(Session::new(off, None, vec![]).is_ok());
}
