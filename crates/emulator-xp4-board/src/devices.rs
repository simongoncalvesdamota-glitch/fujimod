//! Common Device/Host adapter owned by the Unicorn application CPU profile.
//! Devices receive physical RAM, guest time and IRQ lines, never CPU state.
use super::Cluster;
use fujimod_emulator_devices::graphics::scanout::Frame;
use fujimod_emulator_devices::{Cx, Device, Host, Record};
use fujimod_emulator_ip::usb::dwc3::{self, Dwc3};
use fujimod_emulator_xp4_io::adc::Adc;
use fujimod_emulator_xp4_storage::storage::{self, DmaController, SerialFlashController};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

struct Connected<T>(Arc<Mutex<T>>);
impl<T: Device> Device for Connected<T> {
    fn name(&self) -> &'static str {
        self.0.lock().unwrap().name()
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        self.0.lock().unwrap().read(cx, address, size)
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        self.0.lock().unwrap().write(cx, address, size, value);
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        self.0.lock().unwrap().advance(cx);
    }
    fn event(&mut self, cx: &mut Cx<'_>, token: u64) {
        self.0.lock().unwrap().event(cx, token);
    }
    fn pollable(&self, address: u64, size: u8) -> bool {
        self.0.lock().unwrap().pollable(address, size)
    }
    fn set_scene(&mut self, scene: &serde_json::Value) -> Result<bool, String> {
        self.0.lock().unwrap().set_scene(scene)
    }
    fn set_i2c_registers(
        &mut self,
        base: u64,
        address: u16,
        registers: &BTreeMap<u8, u16>,
    ) -> Result<bool, String> {
        self.0.lock().unwrap().set_i2c_registers(base, address, registers)
    }
    fn finish(&mut self) -> Result<(), String> {
        self.0.lock().unwrap().finish()
    }
    fn next_deadline(&self, now: u64) -> Option<u64> {
        self.0.lock().unwrap().next_deadline(now)
    }
    fn save(&self) -> serde_json::Value {
        self.0.lock().unwrap().save()
    }
    fn restore(&mut self, state: &serde_json::Value) -> Result<(), String> {
        self.0.lock().unwrap().restore(state)
    }
}

pub struct Devices {
    models: Vec<Option<Box<dyn Device>>>,
    events: BTreeSet<(u64, usize, u64)>,
    windows: Vec<(u64, u64, usize)>,
    pub log: Vec<Record>,
    prior_records: usize,
    pins: Option<fujimod_emulator_ip::socionext::gpio::SharedPins>,
    gpio_irqs: Vec<(usize, usize, u32, u32)>,
    gpio_irq_levels: bool,
    adc: Option<(usize, Arc<Mutex<Adc>>)>,
    xpp_adc: Option<Arc<Mutex<fujimod_emulator_xpp::adc::Converter>>>,
    usb: Option<Arc<Mutex<Dwc3>>>,
    /// The imaging sensor, for laws a plan changes between slices.
    imaging_sensor: Option<fujimod_emulator_sensor_input::sensor::Sensor>,
    /// The statistics scene of the imaging block, for MOVDC scene pictures.
    imaging_scene: Option<fujimod_emulator_sensor_input::statistics::Statistics>,
    pub frames: BTreeMap<usize, Frame>,
    pub outputs: BTreeMap<usize, serde_json::Value>,
    /// Earliest event or model deadline, cached when `due` found nothing due.
    /// `None` means unknown: any access, external input or deadline work can
    /// move a model's deadline, so each of them clears the cache. A model's
    /// deadline only depends on time as "due now" while a state is pending,
    /// which `due` sees when it recomputes, so a cached future deadline stays
    /// exact until the devices change.
    deadline: Option<Option<u64>>,
}
struct PhysicalHost<'a> {
    cluster: &'a Cluster,
    events: &'a mut BTreeSet<(u64, usize, u64)>,
    log: &'a mut Vec<Record>,
    prior_records: usize,
    frames: &'a mut BTreeMap<usize, Frame>,
    outputs: &'a mut BTreeMap<usize, serde_json::Value>,
}
impl Host for PhysicalHost<'_> {
    fn now(&self) -> u64 {
        unsafe { (*self.cluster.state.get()).ticks }
    }
    fn schedule(&mut self, device: usize, at: u64, token: u64) {
        self.events.insert((at, device, token));
    }
    fn set_irq(&mut self, line: u32, level: bool) {
        unsafe {
            (*self.cluster.state.get())
                .gic
                .set_line(line as usize, level);
        }
    }
    fn raise_irq(&mut self, _cpu: usize, _line: u32) -> Result<(), String> {
        Err("Per-core interrupt input is not wired for ARMv7-A".into())
    }
    // Applied once the MMU is off, as retained TLB entries keep the ROM
    // page table in the old low window until then.
    fn select_memory(&mut self, decoder: usize, choice: usize) -> Result<(), String> {
        let state = unsafe { &mut *self.cluster.state.get() };
        if (decoder, choice) != (0, 1) || state.low_alias.is_none() || state.remap_pending {
            return Err("Unsupported ARMv7-A memory decoder selection".into());
        }
        state.remap_pending = true;
        Ok(())
    }
    fn ram_read(&self, address: u64, out: &mut [u8]) -> Result<(), String> {
        let source = self.cluster.memory_pointer(address, out.len(), 1)?;
        unsafe {
            if let Some(guard) = &mut (*self.cluster.state.get()).driver_guard {
                guard.model_access(address, out.len() as u64, false)?;
            }
            std::ptr::copy_nonoverlapping(source, out.as_mut_ptr(), out.len());
        }
        Ok(())
    }
    fn ram_write(&mut self, address: u64, bytes: &[u8]) -> Result<(), String> {
        let target = self.cluster.memory_pointer(address, bytes.len(), 2)?;
        unsafe {
            if let Some(guard) = &mut (*self.cluster.state.get()).driver_guard {
                guard.model_access(address, bytes.len() as u64, true)?;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            let state = &mut *self.cluster.state.get();
            for monitor in &mut state.exclusive {
                if monitor.is_some_and(|(a, n)| {
                    a < address + bytes.len() as u64 && address < a + n as u64
                }) {
                    *monitor = None;
                }
            }
            state.cache_dirty = true;
        }
        if self.cluster.counted {
            // Counted engines track their translated code byte by byte: tell
            // each one which DRAM/window bytes changed (DRAM identity 0 for the
            // alias), instead of discarding every translation at the flush.
            let identity = match self.cluster.dram_alias {
                Some(alias)
                    if address >= alias && address - alias < self.cluster.ram.len() as u64 =>
                {
                    address - alias
                }
                _ => address,
            };
            let pointer = std::ptr::NonNull::new(target).ok_or("Null device RAM target")?;
            for cpu in &self.cluster.cpus {
                unsafe { cpu.backing_written(identity, pointer, bytes.len()) }?;
            }
        }
        Ok(())
    }
    fn scanout(&mut self, frame: Frame) -> Result<(), String> {
        self.frames.insert(frame.output, frame);
        Ok(())
    }
    fn output_status(&mut self, output: usize, state: serde_json::Value) -> Result<(), String> {
        if state["active"] != true {
            self.frames.remove(&output);
        }
        self.outputs.insert(output, state);
        Ok(())
    }
    fn record(&mut self, record: Record) {
        if self.prior_records + self.log.len() < 65536 {
            self.log.push(record);
        }
    }
}
impl Devices {
    pub fn set_adc_input(
        &mut self,
        c: &Cluster,
        channel: usize,
        sample: u16,
    ) -> Result<(), String> {
        self.changed();
        if let Some(adc) = &self.xpp_adc {
            return adc.lock().unwrap().set_input(channel, sample);
        }
        let (id, adc) = self.adc.as_ref().ok_or("XP4 ADC inputs are not wired")?;
        let mut host = PhysicalHost {
            cluster: c,
            events: &mut self.events,
            log: &mut self.log,
            prior_records: self.prior_records,
            frames: &mut self.frames,
            outputs: &mut self.outputs,
        };
        adc.lock()
            .unwrap()
            .set_input(&mut Cx::new(&mut host, 0, 0, *id), channel, sample)
    }
    /// Sets an outside GPIO level; returns the GIC line its edge pulses, if
    /// the pin is wired to one and the level changed.
    pub fn set_gpio_input(
        &mut self,
        port: usize,
        pin: usize,
        high: bool,
    ) -> Result<Option<u32>, String> {
        self.changed();
        if port >= 16 || pin >= 16 {
            return Err("Invalid XP4 external GPIO pin".into());
        }
        let mut pins = self
            .pins
            .as_ref()
            .ok_or("XP4 GPIO inputs are not wired")?
            .lock()
            .unwrap();
        let was = pins.input(port, pin);
        pins.set_input(port, pin, high);
        Ok(self
            .gpio_irqs
            .iter()
            .find(|&&(p, n, _, _)| (p, n) == (port, pin))
            .map(|&(_, _, falling, rising)| if high { rising } else { falling })
            .filter(|&line| was != high && line != 0))
    }
    /// Switches the GPIO interrupt wiring from edge pulses to line levels.
    pub fn gpio_irq_levels(&mut self) -> Result<(), String> {
        if self.gpio_irqs.is_empty() {
            return Err("GPIO interrupt levels need GPIO interrupt lines".into());
        }
        self.gpio_irq_levels = true;
        Ok(())
    }
    /// With line levels on, the (line, level) pairs a wired pin at `high`
    /// drives: the rising line is asserted while the pin is high, the
    /// falling line while it is low. `None` when levels are off or the pin
    /// is not wired.
    pub fn gpio_line_levels(&self, port: usize, pin: usize, high: bool) -> Option<Vec<(u32, bool)>> {
        if !self.gpio_irq_levels {
            return None;
        }
        self.gpio_irqs
            .iter()
            .find(|&&(p, n, _, _)| (p, n) == (port, pin))
            .map(|&(_, _, falling, rising)| {
                [(falling, !high), (rising, high)]
                    .into_iter()
                    .filter(|&(line, _)| line != 0)
                    .collect()
            })
    }
    /// The lens ready line (`storage_dma_lens.motion_end`): a model sharing
    /// the GPIO pins, driving the ready pin's interrupt lines by level.
    pub fn lens_motion(&mut self, config: crate::definition::LensMotionEnd) -> Result<(), String> {
        let pins = self
            .pins
            .clone()
            .ok_or("The lens motion end requires GPIO wiring")?;
        if self.models.iter().flatten().any(|m| m.name() == "lens-motion-end") {
            return Err("One lens motion end per board".into());
        }
        let lines = match self
            .gpio_irqs
            .iter()
            .find(|&&(p, n, _, _)| (p, n) == config.pin)
        {
            Some(&(_, _, falling, rising)) if self.gpio_irq_levels => (falling, rising),
            Some(_) => {
                return Err("The lens motion end drives its interrupt lines by level: set gpio_irq_levels".into());
            }
            None => (0, 0),
        };
        let model = crate::lens_motion::LensMotion::new(config, pins, lines)?;
        self.changed();
        self.models.push(Some(Box::new(model)));
        Ok(())
    }
    pub fn gpio_irqs(&mut self, wiring: Vec<(usize, usize, u32, u32)>) -> Result<(), String> {
        if self.pins.is_none() || !self.gpio_irqs.is_empty() {
            return Err("GPIO interrupt lines need one GPIO wiring".into());
        }
        let mut seen = BTreeSet::new();
        for &(port, pin, falling, rising) in &wiring {
            if port >= 16
                || pin >= 16
                || !seen.insert((port, pin))
                || [falling, rising]
                    .iter()
                    .any(|&line| line != 0 && !(32..512).contains(&line))
            {
                return Err("Invalid GPIO interrupt line wiring".into());
            }
        }
        self.gpio_irqs = wiring;
        Ok(())
    }
    /// Hands a new scene to every model that draws one (the imaging statistics);
    /// returns how many took it.
    pub fn set_scene(&mut self, scene: &serde_json::Value) -> Result<usize, String> {
        self.changed();
        let mut taken = 0;
        for model in self.models.iter_mut().flatten() {
            if model.set_scene(scene)? {
                taken += 1;
            }
        }
        Ok(taken)
    }
    /// Sets declared registers of an I2C target on the F_I2C channel at `base`, mid-run.
    pub fn set_i2c_registers(
        &mut self,
        base: u64,
        address: u16,
        registers: &BTreeMap<u8, u16>,
    ) -> Result<(), String> {
        self.changed();
        for model in self.models.iter_mut().flatten() {
            if model.set_i2c_registers(base, address, registers)? {
                return Ok(());
            }
        }
        Err(format!("No F_I2C channel at {base:#x}"))
    }
    /// Moves an outside-driven word of a serial part (the power controller's
    /// key group 0 holds the power switch), mid-run.
    /// Encoder pulses into a channel of the 16-channel timer used as a counter
    /// (see `Timers16::add_count`).
    pub fn add_timer_count(&mut self, channel: usize, delta: i32) -> Result<u32, String> {
        self.changed();
        for model in self.models.iter_mut().flatten() {
            let any: &mut dyn std::any::Any = model.as_mut();
            if let Some(t) = any.downcast_mut::<fujimod_emulator_xp4_io::timers16::Timers16>() {
                return t.add_count(channel, delta);
            }
        }
        Err("XP4 16-channel timer is not wired".into())
    }
    pub fn set_power_input(&mut self, register: u8, value: u16) -> Result<(), String> {
        self.changed();
        for model in self.models.iter_mut().flatten() {
            let any: &mut dyn std::any::Any = model.as_mut();
            if let Some(spi) = any.downcast_mut::<fujimod_emulator_xp4_io::spi::Spi>() {
                if spi.set_input(register, value)? {
                    return Ok(());
                }
            }
        }
        Err("No serial part drives this input".into())
    }
    /// Declares a scripted answer on the PL011 UART at `base` (a test
    /// hypothesis about the far end, see `pl011`).
    pub fn uart_reply(&mut self, base: u64, when: Vec<u8>, answer: Vec<u8>) -> Result<(), String> {
        self.changed();
        for model in self.models.iter_mut().flatten() {
            let any: &mut dyn std::any::Any = model.as_mut();
            if let Some(uart) = any.downcast_mut::<fujimod_emulator_xp4_io::pl011::Pl011>() {
                if uart.base() == base {
                    return uart.add_reply(when, answer);
                }
            }
        }
        Err("No PL011 UART at this base".into())
    }
    /// Every non-zero sector of the SD card in the host at `base`, as
    /// (LBA, 512 bytes), in LBA order (for inspecting what the guest wrote).
    pub fn card_sectors(&self, base: u64) -> Result<Vec<(u32, Vec<u8>)>, String> {
        let mut hosts = Vec::new();
        for (id, model) in self.models.iter().enumerate() {
            let Some(model) = model else { continue };
            let any: &dyn std::any::Any = model.as_ref();
            if let Some(host) = any.downcast_ref::<fujimod_emulator_ip::sdhci::host::SdHost>() {
                let mapped = self
                    .windows
                    .iter()
                    .any(|&(lo, hi, w)| w == id && lo <= base && base < hi.max(lo + 0x1000));
                hosts.push((mapped, host));
            }
        }
        let host = match hosts.iter().find(|(mapped, _)| *mapped) {
            Some((_, host)) => *host,
            None if hosts.len() == 1 => hosts[0].1,
            None => return Err(format!("No SD host at this base ({} hosts)", hosts.len())),
        };
        Ok(host
            .card()
            .nonzero_sectors()
            .map(|(lba, bytes)| (lba, bytes.to_vec()))
            .collect())
    }
    /// Declares the input buffer's memory read-back law (event bits of +0x0c,
    /// ticks): a test hypothesis about which event ends a still read-back.
    pub fn input_readback(&mut self, event: u32, ticks: u64) -> Result<(), String> {
        self.changed();
        for model in self.models.iter_mut().flatten() {
            let any: &mut dyn std::any::Any = model.as_mut();
            if let Some(input) =
                any.downcast_mut::<fujimod_emulator_sensor_input::input_buffer::InputBuffer>()
            {
                return input.set_readback(event, ticks);
            }
        }
        Err("No input buffer wired".into())
    }
    /// Attaches an RL78 flash loader (Protocol A) to the PL011 at `base`.
    pub fn uart_rl78(
        &mut self,
        base: u64,
        signature: [u8; fujimod_emulator_xp4_io::rl78::SIGNATURE],
        echo: bool,
    ) -> Result<(), String> {
        self.changed();
        for model in self.models.iter_mut().flatten() {
            let any: &mut dyn std::any::Any = model.as_mut();
            if let Some(uart) = any.downcast_mut::<fujimod_emulator_xp4_io::pl011::Pl011>() {
                if uart.base() == base {
                    return uart.attach_rl78(signature, echo);
                }
            }
        }
        Err("No PL011 UART at this base".into())
    }
    pub fn gpio_registers(&self) -> Option<Vec<[u32; 8]>> {
        let pins = self.pins.as_ref()?.lock().unwrap();
        Some((0..16).map(|port| pins.registers(port)).collect())
    }
    /// Register-1 writes that cleared one GPIO pin so far.
    pub fn gpio_releases(&self, port: usize, pin: usize) -> Option<u64> {
        (port < 16 && pin < 16).then_some(())?;
        Some(self.pins.as_ref()?.lock().unwrap().releases(port, pin))
    }
    pub fn gpio(&mut self, base: u64, inputs: [u16; 16]) -> Result<(), String> {
        if self.pins.is_some()
            || self
                .windows
                .iter()
                .any(|&(lo, hi, _)| base < hi && lo < base + 0x400)
        {
            return Err("Overlapping GPIO wiring".into());
        }
        let pins = fujimod_emulator_ip::socionext::gpio::Pins::new(inputs);
        let id = self.models.len();
        self.models.push(Some(Box::new(
            fujimod_emulator_ip::socionext::gpio::Gpio::new(base, pins.clone())?,
        )));
        self.windows.push((base, base + 0x400, id));
        self.pins = Some(pins);
        Ok(())
    }
    pub fn temperature(&mut self, value: i32, ticks: u64) -> Result<(), String> {
        let ranges = [
            (0xfff4700c, 0xfff47010),
            (0xfff47028, 0xfff4702c),
            (0xfff47810, 0xfff47854),
        ];
        if ranges
            .iter()
            .any(|&(a, b)| self.windows.iter().any(|&(lo, hi, _)| a < hi && lo < b))
        {
            return Err("Overlapping XP4 temperature wiring".into());
        }
        let model = fujimod_emulator_xp4_io::temperature::Temperature::new(value, ticks)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.extend(ranges.map(|(a, b)| (a, b, id)));
        Ok(())
    }
    pub fn power_spi(
        &mut self,
        base: u64,
        idle: bool,
        port: usize,
        pin: usize,
        active_high: bool,
        inputs: BTreeMap<u8, u16>,
        rtc_ticks: u64,
        gauge: Option<(u64, bool, usize, usize, bool, BTreeMap<u8, u16>, Vec<u8>)>,
    ) -> Result<(), String> {
        let pins = self
            .pins
            .as_ref()
            .ok_or("Power SPI requires physical GPIO wiring")?
            .clone();
        let part = fujimod_emulator_xp4_io::power_serial::PowerSerial::new(
            pins.clone(),
            port,
            pin,
            active_high,
            idle,
            inputs,
            rtc_ticks,
        )?;
        let Some((gauge_base, gauge_idle, gauge_port, gauge_pin, gauge_high, gauge_inputs, writable)) =
            gauge
        else {
            return self.spi_input(base, Box::new(part));
        };
        // X-T3: the battery gauge shares the power controller's channel behind
        // its own chip select; both idle low so the bus is their OR.
        if gauge_base != base || idle || gauge_idle || (gauge_port, gauge_pin) == (port, pin) {
            return Err("The XP4 gauge SPI needs the power channel, a distinct chip select and low idle levels".into());
        }
        let gauge = fujimod_emulator_xp4_io::gauge_serial::GaugeSerial::new(
            pins,
            gauge_port,
            gauge_pin,
            gauge_high,
            gauge_idle,
            gauge_inputs,
            writable,
        )?;
        self.spi_input(
            base,
            Box::new(fujimod_emulator_xp4_io::spi::SharedBus(vec![
                Box::new(part),
                Box::new(gauge),
            ])),
        )
    }

    pub fn checksum_power_spi(
        &mut self,
        base: u64,
        idle: bool,
        port: usize,
        pin: usize,
        active_high: bool,
        inputs: BTreeMap<u8, u16>,
        disable_writes: Vec<(u8, u16)>,
        rtc: Option<fujimod_emulator_xp4_io::rtc::StagedConfig>,
        retained_registers: Vec<u8>,
    ) -> Result<(), String> {
        let pins = self
            .pins
            .as_ref()
            .ok_or("Checksum power SPI requires physical GPIO wiring")?
            .clone();
        let part = fujimod_emulator_xp4_io::checksum_power_serial::ChecksumPowerSerial::new(
            pins,
            port,
            pin,
            active_high,
            idle,
            inputs,
            disable_writes,
            rtc,
            retained_registers,
        )?;
        self.spi_input(base, Box::new(part))
    }

    pub fn adc(&mut self, inputs: [u16; 12], sample_ticks: u64) -> Result<(), String> {
        let base = fujimod_emulator_xp4_io::adc::BASE;
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base + 0x48)
        {
            return Err("Overlapping XP4 ADC window".into());
        }
        let model = Arc::new(Mutex::new(Adc::new(inputs, sample_ticks)?));
        let id = self.models.len();
        self.models.push(Some(Box::new(Connected(model.clone()))));
        self.adc = Some((id, model));
        self.windows.push((base, base + 0x48, id));
        Ok(())
    }

    pub fn usb(&mut self, config: dwc3::Config) -> Result<(), String> {
        let model = Dwc3::new(config)?;
        let ranges = [
            (0xc100, 0xc114),
            (0xc200, 0xc204),
            (0xc2c0, 0xc2c4),
            (0xc400, 0xc410),
            (0xc700, 0xc718),
            (0xc720, 0xc724),
            (0xc800, 0xca00),
        ];
        if self.usb.is_some()
            || ranges.iter().any(|&(a, b)| {
                self.windows
                    .iter()
                    .any(|&(lo, hi, _)| config.base + a < hi && lo < config.base + b)
            })
        {
            return Err("Duplicate or overlapping A7 DWC3 wiring".into());
        }
        let id = self.models.len();
        let model = Arc::new(Mutex::new(model));
        self.models.push(Some(Box::new(Connected(model.clone()))));
        self.windows
            .extend(ranges.map(|(a, b)| (config.base + a, config.base + b, id)));
        self.usb = Some(model);
        Ok(())
    }
    pub fn usb_phy(
        &mut self,
        config: fujimod_emulator_xp4_io::usb_phy::Config,
    ) -> Result<(), String> {
        let model = fujimod_emulator_xp4_io::usb_phy::Phy::new(config)?;
        let ranges = [
            (config.base, config.base + 8),
            (config.base + 0x400, config.base + 0x404),
        ];
        if ranges
            .iter()
            .any(|&(a, b)| self.windows.iter().any(|&(lo, hi, _)| a < hi && lo < b))
        {
            return Err("Overlapping XP4 USB PHY wiring".into());
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.extend(ranges.map(|(a, b)| (a, b, id)));
        Ok(())
    }
    fn usb_model(&self) -> Result<std::sync::MutexGuard<'_, Dwc3>, String> {
        Ok(self
            .usb
            .as_ref()
            .ok_or("DWC3 is not wired")?
            .lock()
            .unwrap())
    }
    pub fn usb_link(&self, now: u64, connected: bool) -> Result<(), String> {
        self.usb_model()?.link(now, connected)
    }
    pub fn usb_reset(&self, now: u64) -> Result<(), String> {
        self.usb_model()?.bus_reset(now)
    }
    pub fn usb_transaction(
        &self,
        now: u64,
        endpoint: usize,
        setup: bool,
        out: Option<Vec<u8>>,
    ) -> Result<u64, String> {
        self.usb_model()?.transaction(now, endpoint, setup, out)
    }
    pub fn usb_reply(&self, id: u64) -> Result<Option<dwc3::Reply>, String> {
        Ok(self.usb_model()?.reply(id).cloned())
    }
    pub fn usb_consume(&self, id: u64) -> Result<(), String> {
        self.usb_model()?.consume_reply(id)
    }

    pub fn dmp(&mut self, base: u64, irq: u32) -> Result<(), String> {
        if irq >= 512
            || self
                .windows
                .iter()
                .any(|&(lo, hi, _)| base < hi && lo < base + 0x3000)
        {
            return Err("Invalid or overlapping XP4 DMP wiring".into());
        }
        let model = fujimod_emulator_ip_dmp::smaph::Renderer::new(base, irq)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + 0x3000, id));
        Ok(())
    }
    pub fn lcdc(
        &mut self,
        base: u64,
        pixel_ticks: u64,
        irqs: [u32; 2],
        layout: Option<&str>,
        video_layout: Option<&str>,
    ) -> Result<(), String> {
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base + 0x604)
        {
            return Err("Duplicate XP4 LCDC window".into());
        }
        let picture = match layout {
            None => None,
            Some("bgra8888-straight") => {
                Some(fujimod_framebuffer_xp4::Picture::Bgra8888Straight)
            }
            _ => return Err("Unsupported XP4 LCDC picture layout".into()),
        };
        let video = match video_layout {
            None => None,
            Some("yycbcr422") => Some(fujimod_framebuffer_xp4::Video::YyCbCr422),
            _ => return Err("Unsupported XP4 LCDC video layout".into()),
        };
        let model =
            fujimod_emulator_xp4_display::lcdc::Lcdc::new(base, pixel_ticks, irqs, picture, video)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + 0x604, id));
        Ok(())
    }
    /// Each clock word (SDHCI 0x2C) with the word after it (0x2E: timeout
    /// control and software resets), which the model also owns.
    pub fn sd_clocks(&mut self, words: &[(u64, u64)]) -> Result<(), String> {
        if words.iter().any(|&(address, _)| {
            self.windows
                .iter()
                .any(|&(lo, hi, _)| address < hi && lo < address + 4)
        }) {
            return Err("Overlapping SDHCI clock wiring".into());
        }
        let model = fujimod_emulator_ip::sdhci::Clocks::new(words)?;
        let id = self.models.len();
        self.windows
            .extend(words.iter().map(|&(address, _)| (address, address + 4, id)));
        self.models.push(Some(Box::new(model)));
        Ok(())
    }
    pub fn requests(&mut self, inputs: Vec<(u64, u64)>) -> Result<(), String> {
        let id = self.models.len();
        let model = fujimod_emulator_xp4_io::requests::Requests::new(&inputs)?;
        self.windows
            .extend(inputs.iter().map(|&(a, _)| (a, a + 4, id)));
        self.models.push(Some(Box::new(model)));
        Ok(())
    }
    pub fn movdc(
        &mut self,
        outputs: Vec<(u64, u32, u64, u64, u8)>,
        picture: Option<fujimod_emulator_sensor_input::statistics::ViewPicture>,
        yuv420: bool,
    ) -> Result<(), String> {
        let source = match picture {
            None => None,
            Some(law) => {
                law.check()?;
                let scene = self
                    .imaging_scene
                    .clone()
                    .ok_or("The MOVDC scene picture needs the imaging block")?;
                let source: fujimod_emulator_xp4_display::movdc::Source =
                    Arc::new(move |width, height| {
                        scene.paint_view(width as usize, height as usize, &law)
                    });
                Some(source)
            }
        };
        for (base, irq, reset, period, luma) in outputs {
            if self
                .windows
                .iter()
                .any(|&(lo, hi, _)| base < hi && lo < base + 0x10c)
            {
                return Err("Overlapping XP4 MOVDC window".into());
            }
            let mut model =
                fujimod_emulator_xp4_display::movdc::Movdc::new(base, irq, reset, period, luma)?
                    .latch_configuration();
            if let Some(source) = &source {
                model = model.with_source(source.clone());
            }
            if yuv420 {
                model = model.accept_yuv420();
            }
            let id = self.models.len();
            self.models.push(Some(Box::new(model)));
            self.windows.push((base, base + 0x10c, id));
        }
        Ok(())
    }
    pub fn imaging(
        &mut self,
        map: (u64, u64, Vec<u32>, Vec<u32>, Vec<u32>),
        timing: (u64, u64, u64, u64),
        read_gate: (u16, u8),
        ibuf: Option<(u64, u32, u64, u64, u64, String)>,
        start_pulse: Option<(u32, u32)>,
        statistics_window: Option<u64>,
        dma: Option<[u32; 2]>,
        input_events: Option<u32>,
        still_readout: Option<fujimod_emulator_sensor_input::slvs::StillReadout>,
        raw_scene: bool,
        line_pitch: bool,
        tg_cadence: Option<fujimod_emulator_sensor_input::sensor::TgCadence>,
        exposure: Option<fujimod_emulator_sensor_input::sensor::SensorExposure>,
        statistics_outputs: Option<fujimod_emulator_sensor_input::statistics::StatisticsOutputs>,
    ) -> Result<(), String> {
        use fujimod_emulator_sensor_input::{sensor, slvs};
        if statistics_outputs.is_some() && ibuf.is_none() {
            return Err("Statistics outputs need the input buffer and its SRAM".into());
        }
        if exposure.is_some() && !raw_scene {
            return Err("A sensor exposure law needs the RAW scene".into());
        }
        if raw_scene && (ibuf.is_none() || dma.is_none()) {
            return Err("A RAW scene needs the input buffer scene and read-out DMA".into());
        }
        if statistics_window.is_some() && ibuf.is_none() {
            return Err("A statistics window needs the input buffer and its SRAM".into());
        }
        let input_events = input_events.unwrap_or(0);
        if input_events & !0xff != 0 || (input_events != 0 && ibuf.is_none()) {
            return Err("Input-buffer event bits are 0..7 and need the input buffer".into());
        }
        let (command_base, receiver_base, normal_irqs, error_irqs, timing_irqs) = map;
        let (period, active, command_ticks, reset_ticks) = timing;
        if command_base != 0xfff6c000
            || receiver_base != 0xfff69000
            || normal_irqs.len() != 2
            || error_irqs.len() != 2
            || timing_irqs.len() != 2
            || normal_irqs
                .iter()
                .chain(&error_irqs)
                .chain(&timing_irqs)
                .any(|i| !(32..512).contains(i))
        {
            return Err("Invalid XP4 imaging map".into());
        }
        let sensor = sensor::Sensor::new(
            Some(sensor::ImageRoutes {
                dma,
                timing: timing_irqs.try_into().unwrap(),
                latch_unrouted: true,
                start_pulse,
            }),
            read_gate,
        );
        let painter = sensor.clone();
        self.imaging_sensor = Some(sensor.clone());
        if line_pitch {
            if dma.is_none() {
                return Err("A read-out line pitch needs the read-out DMA".into());
            }
            painter.use_line_pitch();
        }
        if let Some(cadence) = tg_cadence {
            // Checked again here: the definition deserializes the rates as is.
            painter.use_tg_cadence(sensor::TgCadence::new(cadence.clock_hz, cadence.tick_hz)?);
        }
        let command = sensor::Command::new(command_base, sensor.clone(), command_ticks)?;
        let mut receiver = slvs::Receiver::new(
            sensor,
            slvs::Wiring {
                base: receiver_base,
                normal_irqs,
                error_irqs,
                // With routed read-out DMA, each frame end serves both
                // channels; a channel runs only when its read-out is requested
                // (X-T3 5.11 still capture arms channel 0).
                output_channels: if dma.is_some() { vec![0, 1] } else { vec![] },
                timing_output: true,
                control: slvs::Control {
                    run_mask: 0x11,
                    run_value: 0,
                    reset_pulse: 0x10,
                    reset_ticks,
                },
            },
            period,
            active,
        )?;
        if let Some(readout) = still_readout {
            receiver = receiver.with_still_readout(readout)?;
        }
        let mut extra: Vec<(u64, u64, Box<dyn Device>)> = vec![];
        if let Some((base, irq, reset, sram, bank_size, scene)) = ibuf {
            use fujimod_emulator_sensor_input::{
                input_buffer::{InputBuffer, Output, Wiring},
                statistics::{Planes, Statistics},
            };
            if base != 0xfff70000
                || sram != 0xfff10000
                || bank_size != 0x7000
                || !(32..512).contains(&irq)
                || reset == 0
            {
                return Err("Invalid XP4 input-buffer wiring".into());
            }
            let mut statistics = Statistics::new(sram, bank_size as usize)?;
            if let Some(window) = statistics_window {
                statistics = statistics.with_window(
                    usize::try_from(window).map_err(|_| "statistics window overflows")?,
                )?;
            }
            let window = statistics.window() as u64;
            let mut outputs = vec![Output {
                enable_word: 0,
                bit: 0x100,
                low: 0x254,
                high: 0x258,
                high_mask: 3,
                columns: 16,
                rows: 16,
                planes: Planes::Rgb,
            }];
            if let Some(extra) = statistics_outputs {
                extra.check()?;
                // Same enable bit and frame as 0x254 (bit map unknown); each
                // output reads its own destination pair (low, low + 4).
                outputs.extend(extra.outputs().into_iter().map(|(low, columns, rows, planes)| {
                    Output {
                        enable_word: 0,
                        bit: 0x100,
                        low,
                        high: low + 4,
                        high_mask: 3,
                        columns,
                        rows,
                        planes,
                    }
                }));
            }
            statistics.set_scene(
                &serde_json::from_str(&scene).map_err(|e| format!("statistics scene: {e}"))?,
            )?;
            let input = InputBuffer::new(
                Wiring {
                    base,
                    irq,
                    status: 0xc,
                    irq_control: 0x2e4,
                    ready: 0x2e8,
                    max_offset: 0x2e8,
                    write_limit: 0x2e4,
                    ignore_undeclared: true,
                    enables: [0x100 | input_events, 0],
                    causes: [0x100 | input_events, 0],
                    outputs,
                    reset_ticks: reset,
                    period_ticks: None,
                },
                Some(statistics.clone()),
            )?;
            if raw_scene {
                painter.paint_scene(statistics.clone());
            }
            if let Some(exposure) = exposure {
                // Checked again here: the definition deserializes the code as is.
                let mut law =
                    sensor::SensorExposure::new(exposure.gain_ref, exposure.conversion_gain)?;
                if let Some(shutter) = exposure.shutter {
                    law = law.with_shutter(shutter)?;
                }
                painter.use_sensor_exposure(law)?;
            }
            self.imaging_scene = Some(statistics.clone());
            receiver = receiver.with_input_buffer(input.clone());
            extra.push((base, 0x2ec, Box::new(input)));
            extra.push((sram, window, Box::new(statistics)));
        }
        for (base, size, model) in [
            (command_base, 0x244, Box::new(command) as Box<dyn Device>),
            (receiver_base, 0x254, Box::new(receiver) as Box<dyn Device>),
        ]
        .into_iter()
        .chain(extra)
        {
            if self
                .windows
                .iter()
                .any(|&(lo, hi, _)| base < hi && lo < base + size)
            {
                return Err("Overlapping XP4 imaging wiring".into());
            }
            let id = self.models.len();
            self.models.push(Some(model));
            self.windows.push((base, base + size, id));
        }
        Ok(())
    }
    /// Imaging frame periods withheld after each later timing generator
    /// start (needs the cadence law); 0 = none.
    pub fn set_standby_timing(&mut self) -> Result<(), String> {
        self.imaging_sensor
            .as_ref()
            .ok_or("XP4 imaging is not wired")?
            .use_standby_timing();
        self.deadline = None;
        Ok(())
    }
    pub fn set_tg_restart_delay(&mut self, frames: u32) -> Result<(), String> {
        self.imaging_sensor
            .as_ref()
            .ok_or("XP4 imaging is not wired")?
            .set_tg_restart_delay(frames)?;
        self.deadline = None;
        Ok(())
    }
    pub fn empty() -> Self {
        Self {
            models: vec![],
            events: Default::default(),
            windows: vec![],
            log: vec![],
            prior_records: 0,
            pins: None,
            gpio_irqs: Vec::new(),
            gpio_irq_levels: false,
            adc: None,
            xpp_adc: None,
            usb: None,
            imaging_sensor: None,
            imaging_scene: None,
            frames: BTreeMap::new(),
            outputs: BTreeMap::new(),
            deadline: None,
        }
    }
    pub fn io_groups(&mut self, base: u64) {
        let id = self.models.len();
        self.models.push(Some(Box::new(
            fujimod_emulator_devices::keyed_io::KeyedIoGroup::new(base),
        )));
        self.windows.push((base, base + 0xa0, id));
    }
    pub fn decompressor(&mut self, base: u64, irq: u32) {
        let id = self.models.len();
        let high = std::sync::Arc::new(std::sync::Mutex::new([0; 2]));
        self.models.push(Some(Box::new(
            fujimod_emulator_devices::lz_osd::Decompressor::new(base, irq, high, None),
        )));
        self.windows.push((base, base + 0x6c, id));
    }
    pub fn spi(&mut self, base: u64, miso: bool) -> Result<(), String> {
        self.spi_input(base, Box::new(fujimod_emulator_xp4_io::spi::FixedPin(miso)))
    }
    fn spi_input(
        &mut self,
        base: u64,
        input: Box<dyn fujimod_emulator_xp4_io::spi::Input>,
    ) -> Result<(), String> {
        if !(0xfff32000..=0xfff38000).contains(&base)
            || base & 0xfff != 0
            || self
                .windows
                .iter()
                .any(|&(lo, hi, _)| base < hi && lo < base + 0x28)
        {
            return Err("Invalid or duplicate XP4 SPI controller".into());
        }
        let id = self.models.len();
        self.models
            .push(Some(Box::new(fujimod_emulator_xp4_io::spi::Spi::new(
                base, input,
            ))));
        self.windows.push((base, base + 0x28, id));
        Ok(())
    }
    pub fn storage(
        jedec: u32,
        routes: Vec<u32>,
        image: Vec<u8>,
        geometry: (usize, usize, u64, u64),
        irq: u32,
        audio: Option<storage::AudioPacing>,
        lens: Option<crate::definition::StorageDmaLens>,
    ) -> Result<Self, String> {
        let routes: [u32; 16] = routes
            .try_into()
            .map_err(|_| "XP4 DMA requires 16 physical IRQ routes")?;
        if routes.iter().any(|irq| !(32..512).contains(irq)) {
            return Err("Invalid XP4 DMA IRQ route".into());
        }
        if !(32..512).contains(&irq) {
            return Err("Invalid XP4 serial-flash IRQ".into());
        }
        let port = storage::port(
            image,
            jedec,
            storage::Geometry {
                erase_bytes: geometry.0,
                page_bytes: geometry.1,
                erase_ticks: geometry.2,
                page_ticks: geometry.3,
            },
        )?;
        Ok(Self {
            models: vec![
                Some(Box::new(SerialFlashController::new(port.clone(), irq))),
                Some(Box::new({
                    let dma = DmaController::new(port, routes).with_audio(audio)?;
                    match lens {
                        Some(l) => dma.with_peer(
                            l.fifo,
                            Box::new(fujimod_emulator_xp4_storage::lens_peer::XMountPeer::new(l.link)?),
                        )?,
                        None => dma,
                    }
                })),
            ],
            events: Default::default(),
            log: Vec::new(),
            prior_records: 0,
            pins: None,
            gpio_irqs: Vec::new(),
            gpio_irq_levels: false,
            adc: None,
            xpp_adc: None,
            usb: None,
            imaging_sensor: None,
            imaging_scene: None,
            frames: BTreeMap::new(),
            outputs: BTreeMap::new(),
            deadline: None,
            windows: vec![
                (storage::CONTROLLER, storage::CONTROLLER + 0x100, 0),
                (storage::FLASH, storage::FLASH + storage::FLASH_SIZE, 0),
                (storage::DMA, storage::DMA + 0x10000, 1),
                (0xfff4a500, 0xfff4a520, 1),
            ],
        })
    }
    /// X-Processor Pro flash controller, its read aperture and the PL330 that
    /// copies from it. The flash contents are an explicit board input.
    pub fn xpp_storage(
        &mut self,
        storage: super::definition::XppStorage,
        image: Vec<u8>,
        windows: Vec<Arc<dyn fujimod_emulator_ip_arm::pl330::Source>>,
    ) -> Result<(), String> {
        use fujimod_emulator_xpp::flash::{self, Aperture, FlashController};
        struct ApertureSource(Arc<Aperture>);
        impl fujimod_emulator_ip_arm::pl330::Source for ApertureSource {
            fn contains(&self, address: u64, len: usize) -> bool {
                self.0.contains(address, len)
            }
            fn read(&self, address: u64, len: usize) -> Result<Vec<u8>, String> {
                self.0.read(address, len)
            }
            fn writable(&self) -> bool {
                true
            }
            fn write(&self, address: u64, bytes: &[u8]) -> Result<(), String> {
                self.0.store(address, bytes)
            }
        }
        let aperture = Arc::new(Aperture::new(
            storage.aperture,
            storage.aperture_size,
            image,
        )?);
        let (array, nor) = match storage.nor {
            Some(nor) => (
                Some(nor.array),
                BTreeMap::from([(
                    nor.chip_select,
                    fujimod_emulator_devices::parts::micron_nor::Registers::new(nor.registers)?,
                )]),
            ),
            None => (None, BTreeMap::new()),
        };
        let controller = FlashController::new(
            flash::Options {
                base: storage.flash_controller,
                transfer_ticks: 1,
                irq: storage.irq,
                array,
            },
            aperture.clone(),
            nor,
        )?;
        let dma = fujimod_emulator_ip_arm::pl330::Pl330::new(
            storage.dma,
            storage.dma_irq_routes,
            std::iter::once(Arc::new(ApertureSource(aperture.clone())) as Arc<_>)
                .chain(windows)
                .collect(),
            fujimod_emulator_ip_arm::pl330::Throughput {
                setup_ticks: storage.dma_setup_ticks,
                bytes_per_tick: storage.dma_bytes_per_tick,
            },
        )?;
        let ranges = [
            (
                storage.flash_controller,
                storage.flash_controller + flash::REGISTERS,
            ),
            (aperture.base(), aperture.base() + aperture.size()),
            (storage.dma, storage.dma + 0x1000),
        ];
        if ranges
            .iter()
            .any(|&(a, b)| self.windows.iter().any(|&(lo, hi, _)| a < hi && lo < b))
        {
            return Err("Overlapping X-Processor Pro storage window".into());
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(controller)));
        self.models.push(Some(Box::new(dma)));
        self.windows.push((ranges[0].0, ranges[0].1, id));
        self.windows.push((ranges[1].0, ranges[1].1, id));
        self.windows.push((ranges[2].0, ranges[2].1, id + 1));
        Ok(())
    }
    /// One X-Processor Pro serial port with its declared peer.
    pub fn xpp_serial(
        &mut self,
        base: u64,
        peer: fujimod_emulator_xpp::serial::Peer,
        clock: Option<fujimod_emulator_xpp::serial::Clock>,
        replies: BTreeMap<u32, u32>,
    ) -> Result<(), String> {
        use fujimod_emulator_xpp::serial::{DiscoveryPort, REGISTERS};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base + REGISTERS)
        {
            return Err("Overlapping X-Processor Pro serial port".into());
        }
        let mut model = DiscoveryPort::attached(base, peer)?.with_replies(replies)?;
        if let Some(clock) = clock {
            model = model.with_clock(clock)?;
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + REGISTERS, id));
        Ok(())
    }
    pub fn xpp_boot(
        &mut self,
        options: fujimod_emulator_xpp::boot::Options,
        card_strap: bool,
    ) -> Result<
        (
            Vec<fujimod_emulator_xpp::resets::ResetPulses>,
            fujimod_emulator_xpp::resets::ExternalLines,
        ),
        String,
    > {
        let bases: Vec<u64> = [
            options.selector_base,
            options.clock_base,
            options.ddr_base,
            options.remap_base,
        ]
        .into_iter()
        .chain(options.bus_bases.iter().copied())
        .chain(options.memory_port_bases.iter().copied())
        .collect();
        if bases.iter().any(|&base| {
            self.windows
                .iter()
                .any(|&(lo, hi, _)| base < hi && lo < base + 0x1000)
        }) {
            return Err("Overlapping X-Processor Pro boot controller".into());
        }
        let model = fujimod_emulator_xpp::boot::BootController::new(
            options,
            vec![fujimod_emulator_devices::signals::Level::new(card_strap)],
        )?;
        let lines = (0..3)
            .map(|n| model.sd_reset(n))
            .collect::<Result<Vec<_>, _>>()?;
        let external = model.external_lines();
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        for base in bases {
            self.windows.push((base, base + 0x1000, id));
        }
        Ok((lines, external))
    }
    /// X-Processor Pro ADC with declared samples.
    pub fn xpp_adc(
        &mut self,
        base: u64,
        samples: &[u16],
        conversion_ticks: u64,
    ) -> Result<(), String> {
        use fujimod_emulator_xpp::adc::{Converter, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base + WINDOW)
        {
            return Err("Overlapping X-Processor Pro ADC".into());
        }
        let model = Arc::new(Mutex::new(Converter::new(base, samples, conversion_ticks)?));
        let id = self.models.len();
        self.models.push(Some(Box::new(Connected(model.clone()))));
        self.xpp_adc = Some(model);
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn xpp_display_output(
        &mut self,
        output: &crate::definition::XppDisplayOutput,
    ) -> Result<(), String> {
        let (base, size) = (output.base, output.size);
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(size))
        {
            return Err("Overlapping X-Processor Pro display output".into());
        }
        let model = fujimod_emulator_xpp::display::Output::new(
            base,
            size,
            &output.set_bits,
            &output.clear_bits,
            output
                .scanout
                .map(|(output, period)| fujimod_emulator_xpp::display::Scanout { output, period }),
        )?;
        let model = match output.handshake_ticks {
            Some(ticks) => model.with_handshake(ticks)?,
            None => model,
        };
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + size, id));
        Ok(())
    }
    pub fn xpp_clocks(
        &mut self,
        base: u64,
        lock_ticks: u64,
        running: Vec<usize>,
    ) -> Result<(), String> {
        use fujimod_emulator_xpp::clocks::{ClockControl, Options};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(0x1000))
        {
            return Err("Overlapping X-Processor Pro clock bank".into());
        }
        let model = ClockControl::new(Options {
            base,
            lock_ticks,
            running,
        })?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + 0x1000, id));
        Ok(())
    }
    /// The reset/gate bank; returns the SD channel reset lines and the
    /// external interrupt lines.
    pub fn xpp_reset_gates(
        &mut self,
        base: u64,
    ) -> Result<
        (
            Vec<fujimod_emulator_xpp::resets::ResetPulses>,
            fujimod_emulator_xpp::resets::ExternalLines,
        ),
        String,
    > {
        use fujimod_emulator_xpp::resets::{Gates, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping X-Processor Pro reset/gate bank".into());
        }
        let model = Gates::new(base)?;
        let lines = (0..3)
            .map(|channel| model.sd_reset(channel))
            .collect::<Result<Vec<_>, _>>()?;
        let external = model.external_lines();
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok((lines, external))
    }
    /// An X-mount lens on its serial port, sharing `link` with the PL330.
    pub fn xpp_lens(
        &mut self,
        lens: crate::definition::XppLens,
        link: &fujimod_emulator_xpp::lens::LensLink,
        lines: fujimod_emulator_xpp::resets::ExternalLines,
    ) -> Result<(), String> {
        use fujimod_emulator_xpp::lens::{LensPort, Profile, Timing, Wiring};
        use fujimod_emulator_xpp::serial::REGISTERS;
        let base = lens.port;
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base + REGISTERS)
        {
            return Err("Overlapping X-Processor Pro lens port".into());
        }
        let pins = self
            .pins
            .as_ref()
            .ok_or("The lens requires GPIO wiring")?
            .clone();
        let model = LensPort::new(
            link,
            Profile {
                page: lens.page,
                replies: lens.replies.into_iter().collect(),
            },
            Timing {
                ready: lens.ready_ticks,
                release: lens.release_ticks,
                drive: lens.drive_ticks,
                poll: lens.poll_ticks,
            },
            Wiring {
                base,
                power: lens.power,
                ready: lens.ready,
                line: lens.line,
                contacts: lens.contacts,
            },
            pins,
            lines,
        )?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + REGISTERS, id));
        Ok(())
    }
    pub fn xpp_shutter(&mut self, shutter: crate::definition::XppShutter) -> Result<(), String> {
        use fujimod_emulator_xpp::shutter::{Shutter, Timing, Wiring};
        let pins = self
            .pins
            .as_ref()
            .ok_or("The shutter requires GPIO wiring")?
            .clone();
        let model = Shutter::new(
            Wiring {
                mode: shutter.mode,
                motor: shutter.motor,
                sensors: shutter.sensors,
                line: shutter.line,
            },
            Timing {
                step: shutter.step_ticks,
                poll: shutter.poll_ticks,
            },
            shutter.positions,
            pins,
        )?;
        self.models.push(Some(Box::new(model)));
        Ok(())
    }
    pub fn xpp_key_lines(
        &mut self,
        keys: crate::definition::XppKeyLines,
        lines: fujimod_emulator_xpp::resets::ExternalLines,
    ) -> Result<(), String> {
        let pins = self
            .pins
            .as_ref()
            .ok_or("Key lines require GPIO wiring")?
            .clone();
        let model =
            fujimod_emulator_xpp::keys::KeyLines::new(&keys.lines, keys.poll_ticks, pins, lines)?;
        self.models.push(Some(Box::new(model)));
        Ok(())
    }
    pub fn xpp_sd(
        &mut self,
        sd: crate::definition::XppSd,
        reset: Option<fujimod_emulator_xpp::resets::ResetPulses>,
        files: &[(String, Vec<u8>)],
    ) -> Result<(), String> {
        let base = sd.options.base;
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(0x1000))
        {
            return Err("Overlapping X-Processor Pro SD channel".into());
        }
        let card = fujimod_emulator_storage::fat::format(&sd.card, files)?;
        let contacts = fujimod_emulator_devices::parts::card_contacts::Contacts::new(true, false);
        let mut model = fujimod_emulator_xpp::sd::Controller::new(
            sd.options,
            card,
            contacts,
            sd.irq
                .map(fujimod_emulator_devices::bus::InterruptLine::Shared),
        )?;
        if let Some(line) = reset {
            model = model.with_reset(line);
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + 0x1000, id));
        Ok(())
    }
    /// A standard SDHCI host owning its whole 4 KiB page, with a FAT card.
    pub fn sd_host(
        &mut self,
        sd: crate::definition::SdHostInput,
        files: &[(String, Vec<u8>)],
    ) -> Result<(), String> {
        use fujimod_emulator_ip::sdhci::host::{SdHost, Wiring};
        let base = sd.options.base;
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(0x108))
        {
            return Err("Overlapping SDHCI host".into());
        }
        let card = fujimod_emulator_storage::fat::format(&sd.card, files)?;
        let contacts = fujimod_emulator_devices::parts::card_contacts::Contacts::new(true, false);
        let host = match sd.irq {
            Some(irq) => SdHost::new(
                sd.options,
                Wiring {
                    card,
                    contacts,
                    irq: fujimod_emulator_devices::bus::InterruptLine::Shared(irq),
                },
            )?,
            None => SdHost::new_polled(sd.options, card, contacts)?,
        };
        let id = self.models.len();
        self.models.push(Some(Box::new(host)));
        // The SDHCI bank ends at +0x108; vendor words after it (X-T3 +0x1e0,
        // +0x1e4) are declared separately, e.g. as a register block.
        self.windows.push((base, base + 0x108, id));
        Ok(())
    }
    pub fn xpp_register_block(
        &mut self,
        block: crate::definition::XppRegisterBlock,
        image: Option<(u32, [u8; 3])>,
        recc_units: &BTreeMap<u64, fujimod_emulator_xpp::blocks::Published>,
    ) -> Result<(), String> {
        let frames = block.frames.map(|f| fujimod_emulator_xpp::blocks::Frames {
            start: f.start,
            stopped: f.stopped,
            irq: f.irq,
            period: f.period_ticks,
        });
        let mut model =
            fujimod_emulator_xpp::blocks::Block::new(block.base, block.size, block.idle, frames)?;
        if let Some(j) = block.job {
            model = model.with_job(fujimod_emulator_xpp::blocks::Job {
                start: j.start,
                status: j.status,
                stop: j.stop,
                irq: j.irq,
                ticks: j.ticks,
            })?;
            if j.start_clears {
                model = model.with_start_clearing()?;
            }
        }
        if block.own_name {
            model = model.with_own_name();
        }
        if let Some(words) = recc_units.get(&block.base) {
            model = model.with_published(words.clone());
        }
        if block.audio_converter && block.recc.is_some() {
            return Err("A register block is RECC or the audio converter, not both".into());
        }
        if let Some(r) = block.recc {
            model = model.with_work(fujimod_emulator_xpp::blocks::Work::Recc(
                fujimod_emulator_xpp::blocks::Recc {
                    raw: r.raw,
                    site: r.site,
                    area: r.area,
                },
            ))?;
            if let Some(m) = r.movie {
                model = model
                    .with_recc_movie(fujimod_emulator_xpp::blocks::ReccMovie { shift: m.shift, color: m.color })?;
            }
            if let Some(c) = r.firmware_color {
                let unit = |base| {
                    recc_units
                        .get(&base)
                        .cloned()
                        .ok_or("a RECC firmware color unit is not declared")
                };
                model = model.with_recc_color(unit(c.tone)?, unit(c.ycbcr)?, c.chroma_modes)?;
                if c.chroma_matrix {
                    model = model.with_recc_chroma_matrix()?;
                }
                if c.brightness_gain {
                    model = model.with_recc_brightness_gain()?;
                }
                if c.luma_bands {
                    model = model.with_recc_luma_bands()?;
                }
                if let Some(reference) = c.white_balance {
                    model = model.with_recc_white_balance(reference)?;
                }
                if let Some(l) = c.lut3d {
                    let table = l.tables().map(unit).into_iter().collect::<Result<Vec<_>, _>>()?;
                    model = model.with_recc_lut3d(unit(l.control)?, table)?;
                }
            }
        }
        if block.audio_converter {
            model = model.with_work(fujimod_emulator_xpp::blocks::Work::Audio)?;
        }
        if !block.fixed.is_empty() {
            model = model.with_fixed(block.fixed)?;
        }
        if !block.commands.is_empty() {
            model = model.with_commands(
                block
                    .commands
                    .iter()
                    .map(|c| fujimod_emulator_xpp::blocks::Command {
                        word: c.word,
                        value: c.value,
                        status: c.status,
                        mask: c.mask,
                        state: c.state,
                    })
                    .collect(),
            )?;
        }
        if let Some((lines, ycbcr)) = image {
            model = model.with_live_view_image(lines, ycbcr)?;
        }
        let end = block.base + model.window();
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| block.base < hi && lo < end)
        {
            return Err("Overlapping X-Processor Pro register block".into());
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((block.base, end, id));
        Ok(())
    }
    pub fn xpp_statistics(
        &mut self,
        base: u64,
        irq: u32,
        period: u64,
        scene: Option<[u32; 3]>,
        buffers: &[(u64, Vec<u32>)],
    ) -> Result<(), String> {
        use fujimod_emulator_xpp::statistics::{Engine, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping X-Processor Pro statistics engine".into());
        }
        let mut model = Engine::new(base, irq, period)?;
        if let Some(scene) = scene {
            model = model.with_scene(scene);
        }
        for (output, words) in buffers {
            model = model.with_buffer(*output, words)?;
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn xpp_face(&mut self, base: u64, irq: u32, run_ticks: u64) -> Result<(), String> {
        use fujimod_emulator_xpp::face::{Engine, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping X-Processor Pro face detection engine".into());
        }
        let model = Engine::new(base, irq, run_ticks)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    /// DesignWare HDMI TX with no sink attached.
    pub fn xpp_hdmi(&mut self, base: u64) -> Result<(), String> {
        use fujimod_emulator_ip::dw_hdmi::{Transmitter, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping DesignWare HDMI TX".into());
        }
        let model = Transmitter::new(base)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn xpp_power(&mut self, base: u64) -> Result<(), String> {
        use fujimod_emulator_xpp::power::{Control, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping X-Processor Pro power control".into());
        }
        let model = Control::new(base)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn xpp_dram(&mut self, base: u64, refresh_level: u8) -> Result<(), String> {
        use fujimod_emulator_xpp::dram::{Controller, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping X-Processor Pro DRAM controller".into());
        }
        let model = Controller::new(base, refresh_level)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn xpp_i2c(&mut self, bus: crate::definition::XppI2c) -> Result<(), String> {
        use fujimod_emulator_ip::dw_i2c::{Controller, WINDOW};
        let base = bus.options.base;
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping X-Processor Pro I2C bus".into());
        }
        let parts = fujimod_emulator_xpp::i2c::Parts::new(bus.parts)?;
        let model = Controller::new(bus.options, parts)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn job_ack(&mut self, options: fujimod_emulator_xpp::job_ack::Options) -> Result<(), String> {
        let model = fujimod_emulator_xpp::job_ack::JobAck::new(options)?;
        let windows = model.windows();
        if windows.iter().any(|&(base, end)| {
            self.windows.iter().any(|&(lo, hi, _)| base < hi && lo < end)
        }) {
            return Err("Overlapping acknowledged job page".into());
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        for (base, end) in windows {
            self.windows.push((base, end, id));
        }
        Ok(())
    }
    pub fn mali_v61(&mut self, options: fujimod_emulator_xpp::mali_v61::Options) -> Result<(), String> {
        let model = fujimod_emulator_xpp::mali_v61::MaliV61::new(options)?;
        let windows = model.windows();
        if windows.iter().any(|&(base, end)| {
            self.windows.iter().any(|&(lo, hi, _)| base < hi && lo < end)
        }) {
            return Err("Overlapping Mali-V61 page".into());
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        for (base, end) in windows {
            self.windows.push((base, end, id));
        }
        Ok(())
    }
    pub fn jpeg(&mut self, base: u64, irq: u32, ticks: u64, decoder: bool) -> Result<(), String> {
        use fujimod_emulator_xpp::jpeg::{Decoder, Encoder, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping XP4 JPEG engine".into());
        }
        let model: Box<dyn fujimod_emulator_devices::Device> = if decoder {
            Box::new(Decoder::new(base, irq, ticks)?)
        } else {
            Box::new(Encoder::new(base, irq, ticks)?)
        };
        let id = self.models.len();
        self.models.push(Some(model));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn pl011(&mut self, base: u64, irq: Option<u32>) -> Result<(), String> {
        use fujimod_emulator_xp4_io::pl011::{Pl011, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping PL011 UART".into());
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(Pl011::new(base, irq)?)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn timers16(&mut self, base: u64, irq: u32) -> Result<(), String> {
        use fujimod_emulator_xp4_io::timers16::{Timers16, WINDOW};
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping XP4 16-channel timer".into());
        }
        let id = self.models.len();
        self.models.push(Some(Box::new(Timers16::new(base, irq)?)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    pub fn fi2c(&mut self, bus: crate::definition::Fi2c) -> Result<(), String> {
        use fujimod_emulator_ip::socionext::fi2c::{Controller, WINDOW};
        let base = bus.options.base;
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping XP4 F_I2C channel".into());
        }
        let parts = fujimod_emulator_xpp::i2c::Parts::new(bus.parts)?;
        let model = Controller::new(bus.options, parts)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.push((base, base + WINDOW, id));
        Ok(())
    }
    /// The imaging control page, less the frame interrupt words at `hole`
    /// that the frame sync owns.
    pub fn xpp_imaging_control(&mut self, base: u64, hole: Option<u64>) -> Result<(), String> {
        use fujimod_emulator_xpp::imaging::{Control, WINDOW};
        use fujimod_emulator_xpp::sync::INTERRUPT_WINDOW;
        if self
            .windows
            .iter()
            .any(|&(lo, hi, _)| base < hi && lo < base.saturating_add(WINDOW))
        {
            return Err("Overlapping X-Processor Pro imaging control".into());
        }
        let end = base + WINDOW;
        let ranges = match hole {
            None => vec![(base, end)],
            Some(at) if at > base && at & 7 == 0 && at + INTERRUPT_WINDOW < end => {
                vec![(base, at), (at + INTERRUPT_WINDOW, end)]
            }
            Some(_) => return Err("Frame interrupt words outside the imaging control page".into()),
        };
        let model = Control::new(base)?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows
            .extend(ranges.into_iter().map(|(lo, hi)| (lo, hi, id)));
        Ok(())
    }
    pub fn xpp_frame_sync(&mut self, sync: crate::definition::XppFrameSync) -> Result<(), String> {
        use fujimod_emulator_xpp::sync::{Generator, INTERRUPT_WINDOW, WINDOW};
        let ranges = [
            (sync.base, sync.base.saturating_add(WINDOW)),
            (
                sync.interrupts,
                sync.interrupts.saturating_add(INTERRUPT_WINDOW),
            ),
        ];
        if ranges
            .iter()
            .any(|&(a, b)| self.windows.iter().any(|&(lo, hi, _)| a < hi && lo < b))
        {
            return Err("Overlapping X-Processor Pro frame sync".into());
        }
        let model = Generator::new(
            sync.base,
            sync.interrupts,
            sync.frame_lines,
            sync.period_ticks,
        )?;
        let id = self.models.len();
        self.models.push(Some(Box::new(model)));
        self.windows.extend(ranges.map(|(lo, hi)| (lo, hi, id)));
        Ok(())
    }
    /// X-Processor Pro display frame interrupts; the law has no register window.
    pub fn xpp_display_frames(
        &mut self,
        irqs: Vec<u32>,
        period: u64,
        timing: Option<([u64; 2], u64)>,
    ) -> Result<(), String> {
        if let Some((bases, stop_ticks)) = timing {
            let routes: Vec<u8> = irqs
                .iter()
                .map(|&n| u8::try_from(n).map_err(|_| "Display phase IRQ exceeds 255"))
                .collect::<Result<_, _>>()?;
            let routes: [[u8; 3]; 2] = routes
                .chunks(3)
                .map(|r| {
                    r.try_into()
                        .map_err(|_| "Display needs three IRQs per bank")
                })
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .map_err(|_| "Display needs two timing banks")?;
            let model = fujimod_emulator_xp4_display::timing::XppTiming::new(
                bases, routes, period, stop_ticks,
            )?;
            let windows = model.windows();
            if windows.iter().any(|&(base, end)| {
                self.windows
                    .iter()
                    .any(|&(lo, hi, _)| base < hi && lo < end)
            }) {
                return Err("Overlapping display timing bank".into());
            }
            let id = self.models.len();
            self.models.push(Some(Box::new(model)));
            self.windows.extend(windows.map(|(lo, hi)| (lo, hi, id)));
            return Ok(());
        }
        let model = fujimod_emulator_xpp::display::FrameClock::new(irqs, period)?;
        self.models.push(Some(Box::new(model)));
        Ok(())
    }
    fn owner(&self, address: u64, size: u32) -> Option<usize> {
        self.windows
            .iter()
            .find(|&&(lo, hi, _)| lo <= address && address + u64::from(size) <= hi)
            .map(|x| x.2)
    }
    pub fn owns(&self, address: u64, size: u32) -> bool {
        self.owner(address, size).is_some()
    }
    pub fn guard_windows(&self) -> Vec<(u64, u64)> {
        self.windows.iter().map(|&(lo, hi, _)| (lo, hi)).collect()
    }
    pub fn access(
        &mut self,
        c: &Cluster,
        core: usize,
        address: u64,
        size: u32,
        write: bool,
        value: u64,
    ) -> Result<Option<(u64, bool)>, String> {
        self.changed();
        let Some(id) = self.owner(address, size) else {
            return Ok(None);
        };
        let mut model = self.models[id].take().unwrap();
        let mut host = PhysicalHost {
            cluster: c,
            events: &mut self.events,
            log: &mut self.log,
            prior_records: self.prior_records,
            frames: &mut self.frames,
            outputs: &mut self.outputs,
        };
        let mut cx = Cx::new(&mut host, core, 0, id);
        let reply = if write {
            model.write(&mut cx, address, size as u8, value);
            0
        } else {
            model.read(&mut cx, address, size as u8)
        };
        let unmodeled = cx.is_unmodeled();
        let refusal = cx.take_refusal();
        self.models[id] = Some(model);
        if let Some(reason) = refusal
            && !unmodeled
        {
            return Err(reason);
        }
        Ok(Some((reply, unmodeled)))
    }
    pub fn next_deadline(&self, now: u64) -> Option<u64> {
        self.events
            .iter()
            .next()
            .map(|e| e.0)
            .into_iter()
            .chain(
                self.models
                    .iter()
                    .flatten()
                    .filter_map(|m| m.next_deadline(now)),
            )
            .min()
    }
    /// The devices changed outside `due`: forget the cached deadline.
    pub fn changed(&mut self) {
        self.deadline = None;
    }
    /// The next deadline, cached as `due` caches it when nothing is due by
    /// `now` (a due deadline stays uncached for `due` to service).
    pub fn settled_deadline(&mut self, now: u64) -> Option<u64> {
        if let Some(cached) = self.deadline {
            return cached;
        }
        let at = self.next_deadline(now);
        if at.is_none_or(|at| at > now) {
            self.deadline = Some(at);
        }
        at
    }
    /// Whether the cached deadline shows nothing due by `now`.
    pub fn quiet(&self, now: u64) -> bool {
        self.deadline.is_some_and(|at| at.is_none_or(|at| at > now))
    }
    pub fn due(&mut self, c: &Cluster) -> Result<(), String> {
        let now = unsafe { (*c.state.get()).ticks };
        if self.quiet(now) {
            return Ok(());
        }
        self.deadline = None;
        for _ in 0..64 {
            let event = self.events.iter().next().copied().filter(|e| e.0 <= now);
            let id = event.map(|e| e.1).or_else(|| {
                self.models.iter().position(|m| {
                    m.as_ref()
                        .is_some_and(|m| m.next_deadline(now).is_some_and(|at| at <= now))
                })
            });
            let Some(id) = id else {
                self.deadline = Some(self.next_deadline(now));
                return Ok(());
            };
            let mut model = self.models[id].take().unwrap();
            if let Some(e) = event {
                self.events.remove(&e);
            }
            let mut host = PhysicalHost {
                cluster: c,
                events: &mut self.events,
                log: &mut self.log,
                prior_records: self.prior_records,
                frames: &mut self.frames,
                outputs: &mut self.outputs,
            };
            let mut cx = Cx::new(&mut host, 0, 0, id);
            if let Some((_, _, token)) = event {
                model.event(&mut cx, token);
            } else {
                model.advance(&mut cx);
            }
            let refusal = cx.take_refusal();
            self.models[id] = Some(model);
            if let Some(reason) = refusal {
                return Err(reason);
            }
        }
        Err("ARMv7-A device made no deadline progress".into())
    }
    pub fn snapshots(&self) -> std::collections::BTreeMap<String, String> {
        self.models
            .iter()
            .flatten()
            .map(|m| (m.name().into(), m.save().to_string()))
            .collect()
    }
}

impl Cluster {
    pub fn devices_due(&self) -> Result<(), String> {
        // Same early exit as `Devices::due`, without moving the device set
        // out of the run state and back (two large copies per instruction).
        let now = unsafe { (*self.state.get()).ticks };
        if unsafe { (*self.state.get()).devices.as_ref() }.is_some_and(|d| d.quiet(now)) {
            return Ok(());
        }
        let devices = unsafe { (*self.state.get()).devices.take() };
        if let Some(mut devices) = devices {
            let result = devices.due(self);
            unsafe {
                (*self.state.get()).devices = Some(devices);
            }
            result?;
        }
        Ok(())
    }
    pub fn device_access(
        &self,
        core: usize,
        address: u64,
        size: u32,
        write: bool,
        value: u64,
    ) -> Result<Option<(u64, bool)>, String> {
        let devices = unsafe { (*self.state.get()).devices.take() };
        let Some(mut devices) = devices else {
            return Ok(None);
        };
        let result = devices.access(self, core, address, size, write, value);
        unsafe {
            (*self.state.get()).devices = Some(devices);
        }
        result
    }
    pub fn flush_dma(&self) -> Result<(), String> {
        let state = unsafe { &mut *self.state.get() };
        if state.remap_ready
            && let Some(index) = state.low_alias.take()
        {
            let low = self.memory[index].data.len();
            for cpu in &self.cpus {
                cpu.unmap(0, low)?;
                unsafe {
                    cpu.map_backing(
                        0,
                        low,
                        7,
                        std::ptr::NonNull::new(self.ram.as_ptr()).unwrap(),
                    )
                }?;
            }
            state.remap_pending = false;
            state.remap_ready = false;
        }
        if unsafe { (*self.state.get()).cache_dirty } {
            // Counted engines already invalidated exactly the code a device
            // write touched (`ram_write`); the redirect still ended the slice.
            for cpu in self.cpus.iter().filter(|_| !self.counted) {
                cpu.invalidate_code(None)?;
            }
            unsafe {
                (*self.state.get()).cache_dirty = false;
            }
        }
        Ok(())
    }
}
