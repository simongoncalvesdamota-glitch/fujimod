//! Lens ready line of the XP4 X-mount (X-T3 5.11: GPIO 15.1, lens power 15.0).
//!
//! The body powers the lens by driving the power pin as a high output
//! (0x10715e0(0xf0000, 1) then 0x107159c(0xf0000, 1)) and cuts it by
//! releasing that pin. After power-up the lens pulls the ready pin low
//! (the body's start-up wait, callback 0x10c0188, cancels its timeout
//! 0x10c01c4 on that falling edge) and lets it rise when its initialization
//! is done (rising wait 0x10bfd7c). From then on the body releases the ready
//! pin (0x10b1680(0) → 0x107159c(0xf0001, 0)) before each request that moves
//! the lens and waits for the rising edge (IRQ 166, filter 0x10b1b50,
//! completion 0x10b1b08). The lens answers each release by holding the pin
//! low for its drive time. Releases during start-up or without power are not
//! answered. The pin's interrupt lines follow its level (`gpio_irq_levels`).
//! The lens samples the pins once per declared poll period; delays are
//! declared inputs, not measurements.
use crate::definition::LensMotionEnd;
use fujimod_emulator_devices::{Cx, Device};
use fujimod_emulator_ip::socionext::gpio::SharedPins;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    powered: bool,
    started: bool,
    ready_at: Option<u64>,
    release_at: Option<u64>,
    drive_until: Option<u64>,
    poll_at: u64,
    /// Ready-pin releases already seen (answered or ignored).
    seen: u64,
    drives: u64,
}

pub struct LensMotion {
    config: LensMotionEnd,
    pins: SharedPins,
    /// The ready pin's (falling, rising) level lines, 0 = not wired.
    lines: (u32, u32),
    line: Line,
}

impl LensMotion {
    pub fn new(config: LensMotionEnd, pins: SharedPins, lines: (u32, u32)) -> Result<Self, String> {
        let pin = |(port, pin): (usize, usize)| port < 16 && pin < 16;
        if !pin(config.pin)
            || !pin(config.power)
            || config.pin == config.power
            || [
                config.ready_ticks,
                config.release_ticks,
                config.drive_ticks,
                config.poll_ticks,
            ]
            .contains(&0)
        {
            return Err("Invalid lens motion end".into());
        }
        let seen = pins.lock().unwrap().releases(config.pin.0, config.pin.1);
        Ok(Self {
            config,
            pins,
            lines,
            line: Line {
                seen,
                ..Line::default()
            },
        })
    }
    fn powered(&self) -> bool {
        let (port, pin) = self.config.power;
        let regs = self.pins.lock().unwrap().registers(port);
        regs[1] & regs[0] & (1 << pin) != 0
    }
    fn releases(&self) -> u64 {
        let (port, pin) = self.config.pin;
        self.pins.lock().unwrap().releases(port, pin)
    }
    fn set_ready(&mut self, cx: &mut Cx<'_>, high: bool) {
        let (port, pin) = self.config.pin;
        self.pins.lock().unwrap().set_input(port, pin, high);
        let (falling, rising) = self.lines;
        if falling != 0 {
            cx.set_irq(falling, !high);
        }
        if rising != 0 {
            cx.set_irq(rising, high);
        }
    }
}

impl Device for LensMotion {
    fn name(&self) -> &'static str {
        "lens-motion-end"
    }
    fn read(&mut self, cx: &mut Cx<'_>, _address: u64, _size: u8) -> u64 {
        cx.unmodeled();
        0
    }
    fn write(&mut self, cx: &mut Cx<'_>, _address: u64, _size: u8, _value: u64) {
        cx.unmodeled();
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        let now = cx.now();
        let poll = self.config.poll_ticks;
        if now >= self.line.poll_at {
            self.line.poll_at = (now / poll + 1) * poll;
            let releases = self.releases();
            match (self.powered(), self.line.powered) {
                (true, false) => {
                    self.line.powered = true;
                    self.line.ready_at = now.checked_add(self.config.ready_ticks);
                }
                (false, true) => {
                    self.line = Line {
                        poll_at: self.line.poll_at,
                        drives: self.line.drives,
                        ..Line::default()
                    };
                    self.set_ready(cx, true);
                }
                _ => {}
            }
            if releases > self.line.seen && self.line.started {
                self.line.drives += 1;
                self.line.drive_until = now.checked_add(self.config.drive_ticks);
                self.set_ready(cx, false);
            }
            self.line.seen = releases;
        }
        if self.line.ready_at.is_some_and(|at| now >= at) {
            self.line.ready_at = None;
            self.line.release_at = now.checked_add(self.config.release_ticks);
            self.set_ready(cx, false);
        }
        if self.line.release_at.is_some_and(|at| now >= at) {
            self.line.release_at = None;
            self.line.started = true;
            self.line.seen = self.releases();
            self.set_ready(cx, true);
        }
        if self.line.drive_until.is_some_and(|at| now >= at) {
            self.line.drive_until = None;
            self.set_ready(cx, true);
        }
    }
    fn next_deadline(&self, _now: u64) -> Option<u64> {
        [
            Some(self.line.poll_at),
            self.line.ready_at,
            self.line.release_at,
            self.line.drive_until,
        ]
        .into_iter()
        .flatten()
        .min()
    }
    fn save(&self) -> serde_json::Value {
        serde_json::json!({ "config": self.config, "line": self.line })
    }
    fn checkpoint(&self) -> Result<fujimod_emulator_devices::checkpoint::Checkpoint, String> {
        fujimod_emulator_devices::checkpoint::Checkpoint::encode(self.name(), &(self.config, self.line))
    }
    fn validate_checkpoint(
        &self,
        saved: &fujimod_emulator_devices::checkpoint::Checkpoint,
    ) -> Result<(), String> {
        let (config, _): (LensMotionEnd, Line) = saved.decode(self.name())?;
        if config != self.config {
            return Err("Lens motion end checkpoint configuration differs".into());
        }
        Ok(())
    }
    fn restore_checkpoint(
        &mut self,
        saved: &fujimod_emulator_devices::checkpoint::Checkpoint,
    ) -> Result<(), String> {
        self.validate_checkpoint(saved)?;
        let (_, line): (LensMotionEnd, Line) = saved.decode(self.name())?;
        // The GPIO release count is kept out of saved state: follow the
        // live count rather than answer releases from before the restore.
        self.line = Line {
            seen: self.releases(),
            ..line
        };
        Ok(())
    }
    fn fingerprint(&self) -> Result<serde_json::Value, String> {
        Ok(self.save())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::{Host, Record};
    use fujimod_emulator_ip::socionext::gpio::{Gpio, Pins};
    use std::collections::BTreeMap;

    const BASE: u64 = 0xfff41000;
    const CONFIG: LensMotionEnd = LensMotionEnd {
        pin: (15, 1),
        power: (15, 0),
        ready_ticks: 1000,
        release_ticks: 500,
        drive_ticks: 400,
        poll_ticks: 10,
    };

    /// Board time and the level of each interrupt line.
    #[derive(Default)]
    struct Board {
        now: u64,
        levels: BTreeMap<u32, bool>,
    }
    impl Host for Board {
        fn now(&self) -> u64 {
            self.now
        }
        fn schedule(&mut self, _: usize, _: u64, _: u64) {}
        fn set_irq(&mut self, line: u32, level: bool) {
            self.levels.insert(line, level);
        }
        fn raise_irq(&mut self, _: usize, _: u32) -> Result<(), String> {
            Err("not wired".into())
        }
        fn ram_read(&self, _: u64, _: &mut [u8]) -> Result<(), String> {
            Err("no RAM".into())
        }
        fn ram_write(&mut self, _: u64, _: &[u8]) -> Result<(), String> {
            Err("no RAM".into())
        }
        fn record(&mut self, _: Record) {}
    }
    fn write(gpio: &mut Gpio, b: &mut Board, address: u64, value: u32) {
        let mut cx = Cx::new(b, 0, 0, 0);
        gpio.write(&mut cx, address, 4, u64::from(value));
        assert_eq!(cx.refusal(), None);
    }
    fn run_to(lens: &mut LensMotion, b: &mut Board, until: u64) {
        while let Some(at) = lens.next_deadline(b.now).filter(|&at| at <= until) {
            b.now = at.max(b.now);
            let mut cx = Cx::new(b, 0, 0, 0);
            lens.advance(&mut cx);
            assert_eq!(cx.refusal(), None);
        }
        b.now = until;
    }
    fn ready(pins: &SharedPins) -> bool {
        pins.lock().unwrap().input(15, 1)
    }
    const DIRECTION: u64 = BASE + 15 * 64 + 4;
    const OUTPUT: u64 = BASE + 15 * 64;

    #[test]
    fn a_powered_lens_starts_up_then_answers_each_release_with_one_drive() {
        let pins = Pins::new([0; 16]);
        pins.lock().unwrap().set_input(15, 1, true);
        let mut gpio = Gpio::new(BASE, pins.clone()).unwrap();
        let mut b = Board::default();
        let mut lens = LensMotion::new(CONFIG, pins.clone(), (182, 166)).unwrap();
        // Unpowered: a release (X-T3 46.6 M) is not answered.
        write(&mut gpio, &mut b, DIRECTION, 0x0002_0000);
        run_to(&mut lens, &mut b, 200);
        assert!(ready(&pins));
        // Power on: output high, then output direction (101.15 M).
        write(&mut gpio, &mut b, OUTPUT, 0x0001_0001);
        write(&mut gpio, &mut b, DIRECTION, 0x0001_0001);
        // The start-up release (101.18 M) is not answered either. The
        // power is seen at the next poll (210): the line falls at 1210.
        write(&mut gpio, &mut b, DIRECTION, 0x0002_0000);
        run_to(&mut lens, &mut b, 1205);
        assert!(ready(&pins), "not before ready_ticks after the poll");
        run_to(&mut lens, &mut b, 1215);
        assert!(!ready(&pins), "start-up holds the line low");
        assert_eq!((b.levels.get(&182), b.levels.get(&166)), (Some(&true), Some(&false)));
        run_to(&mut lens, &mut b, 1730);
        assert!(ready(&pins), "start-up done");
        assert_eq!((b.levels.get(&182), b.levels.get(&166)), (Some(&false), Some(&true)));
        // A release (a lens request) holds the line low for one drive.
        write(&mut gpio, &mut b, DIRECTION, 0x0002_0000);
        run_to(&mut lens, &mut b, 1745);
        assert!(!ready(&pins));
        run_to(&mut lens, &mut b, 2135);
        assert!(!ready(&pins));
        run_to(&mut lens, &mut b, 2160);
        assert!(ready(&pins));
        assert_eq!(lens.line.drives, 1);
        // Two releases within one poll period make one drive.
        write(&mut gpio, &mut b, DIRECTION, 0x0002_0000);
        write(&mut gpio, &mut b, DIRECTION, 0x0002_0000);
        run_to(&mut lens, &mut b, 2700);
        assert!(ready(&pins));
        assert_eq!(lens.line.drives, 2);
        // Power off: the line returns high; a release is not answered.
        write(&mut gpio, &mut b, DIRECTION, 0x0001_0000);
        write(&mut gpio, &mut b, DIRECTION, 0x0002_0000);
        run_to(&mut lens, &mut b, 3000);
        assert!(ready(&pins));
        assert!(!lens.line.powered && !lens.line.started);
        assert_eq!(lens.line.drives, 2);
    }

    #[test]
    fn checkpoints_keep_a_pending_drive_and_refuse_another_configuration() {
        let pins = Pins::new([0; 16]);
        let mut gpio = Gpio::new(BASE, pins.clone()).unwrap();
        let mut b = Board::default();
        let mut lens = LensMotion::new(CONFIG, pins.clone(), (0, 0)).unwrap();
        write(&mut gpio, &mut b, OUTPUT, 0x0001_0001);
        write(&mut gpio, &mut b, DIRECTION, 0x0001_0001);
        run_to(&mut lens, &mut b, 1600);
        write(&mut gpio, &mut b, DIRECTION, 0x0002_0000);
        run_to(&mut lens, &mut b, 1620);
        let saved = lens.checkpoint().unwrap();
        let due = lens.line.drive_until;
        assert!(due.is_some());
        let mut other = LensMotion::new(
            LensMotionEnd {
                drive_ticks: 401,
                ..CONFIG
            },
            pins.clone(),
            (0, 0),
        )
        .unwrap();
        assert!(other.restore_checkpoint(&saved).is_err());
        let mut fresh = LensMotion::new(CONFIG, pins.clone(), (0, 0)).unwrap();
        fresh.restore_checkpoint(&saved).unwrap();
        assert_eq!(fresh.line.drive_until, due);
        assert_eq!(fresh.save(), lens.save());
    }

    #[test]
    fn invalid_configurations_are_refused() {
        let pins = Pins::new([0; 16]);
        for bad in [
            LensMotionEnd { poll_ticks: 0, ..CONFIG },
            LensMotionEnd { drive_ticks: 0, ..CONFIG },
            LensMotionEnd { pin: (16, 1), ..CONFIG },
            LensMotionEnd { power: (15, 1), ..CONFIG },
        ] {
            assert!(LensMotion::new(bad, pins.clone(), (0, 0)).is_err());
        }
    }

    /// The board's own wiring: without `motion_end` no model is added, so
    /// there is no lens state, no deadline and nothing to move the ready
    /// pin's interrupt lines; with it, both appear.
    #[test]
    fn without_the_option_the_board_has_no_lens_state_and_no_deadline() {
        let wired = || {
            let mut devices = crate::devices::Devices::empty();
            devices.gpio(BASE, [0; 16]).unwrap();
            devices.gpio_irqs(vec![(15, 1, 182, 166)]).unwrap();
            devices.gpio_irq_levels().unwrap();
            devices
        };
        let off = wired();
        assert!(!off.snapshots().contains_key("lens-motion-end"));
        assert_eq!(off.next_deadline(0), None);

        let mut on = wired();
        on.lens_motion(CONFIG).unwrap();
        assert!(on.snapshots().contains_key("lens-motion-end"));
        assert_eq!(on.next_deadline(0), Some(0), "first poll at once");
        assert!(on.lens_motion(CONFIG).is_err(), "one model per board");

        // A wired ready pin is driven by level only.
        let mut edges = crate::devices::Devices::empty();
        edges.gpio(BASE, [0; 16]).unwrap();
        edges.gpio_irqs(vec![(15, 1, 182, 166)]).unwrap();
        assert!(edges.lens_motion(CONFIG).is_err());
        assert!(crate::devices::Devices::empty().lens_motion(CONFIG).is_err());
    }

    #[test]
    fn the_definition_names_every_delay() {
        let parsed: LensMotionEnd = serde_json::from_str(
            r#"{"pin":[15,1],"power":[15,0],"ready_ticks":1000,"release_ticks":500,"drive_ticks":400,"poll_ticks":10}"#,
        )
        .unwrap();
        assert_eq!(parsed, CONFIG);
        assert!(serde_json::from_str::<LensMotionEnd>(r#"{"pin":[15,1],"ticks":410000}"#).is_err());
    }
}
