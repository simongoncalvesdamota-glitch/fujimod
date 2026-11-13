use fujimod_emulator_xp4_board::{Definition, Session};
use serde_json::json;

fn session() -> Session {
    let definition: Definition = serde_json::from_value(json!({
        "ram_size": 65536, "cores": 1, "discovery": false, "cbar": 0xffff0000u32,
        "memory_regions": [{"base": 0xfff00000u32, "size": 4096, "permissions": 5}],
        "initial_registers": [[[{"Arm32": {"R": 15}}, 0xfff00000u32]]]
    }))
    .unwrap();
    let session = Session::new(definition, None, vec![]).unwrap();
    // b . (spin in place)
    session.write(0xfff00000, &0xeafffffeu32.to_le_bytes()).unwrap();
    session
}

fn ticks_after(session: &Session, instructions: usize) -> u64 {
    let before = session.state(false).virtual_ticks;
    session.run(0, instructions).unwrap();
    session.state(false).virtual_ticks - before
}

#[test]
fn the_time_base_can_change_between_slices() {
    let session = session();
    assert_eq!(ticks_after(&session, 400), 400);
    session.set_time_base((4, 1), false).unwrap();
    assert_eq!(ticks_after(&session, 400), 100);
    // One runnable core: per-core time counts each instruction in full.
    session.set_time_base((10, 1), true).unwrap();
    assert_eq!(ticks_after(&session, 400), 40);
    assert!(session.set_time_base((1, 4), false).is_err());
    assert!(session.set_time_base((0, 1), false).is_err());
    // A refused change keeps the previous time base.
    assert_eq!(ticks_after(&session, 400), 40);
}
