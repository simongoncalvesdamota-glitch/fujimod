//! `movdc_yuv420`: a definition key that needs MOVDC outputs and combines
//! with the `movdc_scene` source of the imaging block.
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

/// The X-T3 5.11 MOVDC and imaging wiring of plans V93/V95.
fn movdc() -> Value {
    json!([[0xfff72000u32, 107, 8, 900900, 128], [0xfff73000u32, 108, 8, 900900, 128]])
}
fn imaging() -> Value {
    json!([[0xfff6c000u32, 0xfff69000u32, [296, 298], [297, 297], [102, 100]],
           [900900, 450450, 5400, 8], [81, 8],
           [0xfff70000u32, 106, 8, 0xfff10000u32, 0x7000,
            "{\"name\": \"functional uniform grey\", \"luma\": 2048, \"samples_per_cell\": 1024, \"black_level\": 1024, \"signal_shift\": 2}"]])
}

#[test]
fn without_movdc_outputs_the_format_is_refused() {
    assert!(Session::new(definition(json!({"movdc_yuv420": true})), None, vec![]).is_err());
    assert!(Session::new(definition(json!({"movdc_yuv420": false})), None, vec![]).is_ok());
}

#[test]
fn the_format_combines_with_and_without_the_scene_source() {
    for extra in [
        json!({"movdc": movdc(), "movdc_yuv420": true}),
        json!({"movdc": movdc(), "imaging": imaging(), "movdc_scene": {}, "movdc_yuv420": true}),
        json!({"movdc": movdc(), "imaging": imaging(), "movdc_scene": {}}),
        json!({"movdc": movdc()}),
    ] {
        Session::new(definition(extra.clone()), None, vec![]).unwrap_or_else(|e| panic!("{extra}: {e}"));
    }
    // The scene still needs the imaging block, with or without the format.
    let bad = json!({"movdc": movdc(), "movdc_scene": {}, "movdc_yuv420": true});
    assert!(Session::new(definition(bad), None, vec![]).is_err());
}
