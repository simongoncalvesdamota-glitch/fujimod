//! The `through_exclusives` slice policy: exclusives, CLREX and identity
//! reads keep the scheduler slice; WFI/WFE/SEV still end it. Run once as is
//! (per-instruction hook) and once with FUJIMOD_XP4_COUNTED=1 (counted).
use fujimod_emulator_cpu::{Register, arm};
use fujimod_emulator_xp4_board::{Definition, Session};
use serde_json::json;

/// mov r1, #0x100 ; then the loop `words` ; then b back to the loop start.
fn session(words: &[u32]) -> Session {
    let definition: Definition = serde_json::from_value(json!({
        "ram_size": 65536, "cores": 1, "discovery": false, "cbar": 0xffff0000u32,
        "memory_regions": [{"base": 0xfff00000u32, "size": 4096, "permissions": 5}],
        "initial_registers": [[[{"Arm32": {"R": 15}}, 0xfff00000u32]]]
    }))
    .unwrap();
    let session = Session::new(definition, None, vec![]).unwrap();
    let mut code = vec![0xe3a01c01u32]; // mov r1, #0x100
    code.extend_from_slice(words);
    // b <loop start = 0xfff00004>
    let at = 4 * code.len() as i32;
    code.push(0xea000000 | (((4 - at - 8) >> 2) as u32 & 0x00ff_ffff));
    let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    session.write(0xfff00000, &bytes).unwrap();
    session.write(0x100, &0x1234_5678u32.to_le_bytes()).unwrap();
    session
}

const LDREX_R0_R1: u32 = 0xe1910f9f;
const STREX_R3_R0_R1: u32 = 0xe1813f90;
const CLREX: u32 = 0xf57ff01f;
const MPIDR_R2: u32 = 0xee102fb0;
const CBAR_R4: u32 = 0xee9f4f10;
const WFI: u32 = 0xe320f003;

fn register(session: &Session, n: u8) -> u64 {
    session.read_register(0, Register::Arm32(arm::Register::R(n))).unwrap()
}

#[test]
fn by_default_an_exclusive_or_identity_read_ends_the_slice() {
    let session = session(&[LDREX_R0_R1, MPIDR_R2]);
    session.run(0, 1000).unwrap();
    // mov, then ldrex: the board performs it and the slice ends there.
    assert_eq!(session.instructions(), vec![2]);
    assert_eq!(register(&session, 0), 0x1234_5678);
    assert_eq!(register(&session, 15), 0xfff00008);
    session.run(0, 1000).unwrap();
    assert_eq!(session.instructions(), vec![3]);
    assert_eq!(register(&session, 2), 0x8000_0000);
}

#[test]
fn through_exclusives_keeps_the_whole_slice() {
    let words = [LDREX_R0_R1, STREX_R3_R0_R1, CLREX, MPIDR_R2, CBAR_R4];
    let session = session(&words);
    session.set_through_exclusives(true).unwrap();
    session.run(0, 1000).unwrap();
    assert_eq!(session.instructions(), vec![1000]);
    // 1 + 166 loops of 6, then 3 more: ldrex, strex, clrex done.
    assert_eq!(register(&session, 15), 0xfff00010);
    assert_eq!(register(&session, 0), 0x1234_5678);
    assert_eq!(register(&session, 3), 0); // strex succeeded
    assert_eq!(register(&session, 2), 0x8000_0000);
    assert_eq!(register(&session, 4), 0xffff0000);
    // The same instructions, slice by slice, without the policy.
    let reference = session_without_policy(&words, 1000);
    assert_eq!(reference.instructions(), vec![1000]);
    for n in 0..16 {
        assert_eq!(register(&session, n), register(&reference, n), "r{n}");
    }
    // Off again: the next exclusive ends the slice.
    session.set_through_exclusives(false).unwrap();
    session.run(0, 1000).unwrap();
    assert_eq!(session.instructions(), vec![1001]); // mpidr alone
    assert_eq!(register(&session, 15), 0xfff00014);
}

fn session_without_policy(words: &[u32], total: u64) -> Session {
    let session = session(words);
    while session.instructions()[0] < total {
        let left = (total - session.instructions()[0]) as usize;
        session.run(0, left).unwrap();
    }
    session
}

#[test]
fn wfi_still_ends_the_slice_under_the_policy() {
    let session = session(&[LDREX_R0_R1, MPIDR_R2, WFI]);
    session.set_through_exclusives(true).unwrap();
    session.run(0, 1000).unwrap();
    assert_eq!(session.instructions(), vec![4]);
    assert_eq!(register(&session, 15), 0xfff00010);
}
