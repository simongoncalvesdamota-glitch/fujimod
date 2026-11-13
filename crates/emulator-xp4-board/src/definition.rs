//! Explicit XP4 processor, wiring and external input selection.
use super::*;
use serde::Deserialize;

fn instruction_rate() -> (u64, u64) {
    (1, 1)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub ram_size: usize,
    pub cores: usize,
    pub discovery: bool,
    #[serde(default)]
    pub memory_regions: Vec<super::memory::Region>,
    #[serde(default)]
    pub initial_registers: Vec<Vec<(Register, u64)>>,
    #[serde(default)]
    pub labels: Vec<u32>,
    #[serde(default)]
    pub cbar: Option<u32>,
    #[serde(default)]
    pub timer_base: Option<u32>,
    #[serde(default)]
    pub timer_layout: Option<TimerLayout>,
    #[serde(default)]
    pub timer_routes: Vec<(usize, usize)>,
    #[serde(default)]
    pub storage_jedec: Option<u32>,
    #[serde(default)]
    pub storage_dma_routes: Vec<u32>,
    #[serde(default)]
    pub spi_inputs: Vec<(u64, bool)>,
    #[serde(default)]
    pub control_requests: Vec<(u64, u64)>,
    #[serde(default)]
    pub storage_geometry: Option<(usize, usize, u64, u64)>,
    #[serde(default)]
    pub storage_irq: Option<u32>,
    /// Movie audio pacing of the XP4 storage DMA (absent: unchanged).
    #[serde(default)]
    pub storage_dma_audio: Option<fujimod_emulator_xp4_storage::storage::AudioPacing>,
    /// X-mount lens behind a peripheral data FIFO of the XP4 storage DMA
    /// (X-T3 5.11: SPI0 data 0xfff32008; absent: the FIFO stays silent).
    #[serde(default)]
    pub storage_dma_lens: Option<StorageDmaLens>,
    #[serde(default)]
    pub xpp_storage: Option<XppStorage>,
    #[serde(default)]
    pub xpp_serial: Vec<u64>,
    #[serde(default)]
    pub xpp_sub_cpu: Option<XppSubCpu>,
    #[serde(default)]
    pub xpp_battery_gauge: Option<XppBatteryGauge>,
    /// X-Processor Pro serial ports with nothing attached: every byte
    /// received reads 0xff.
    #[serde(default)]
    pub xpp_idle_serial: Vec<u64>,
    /// X-mount lens on an X-Processor Pro serial port; requires
    /// `xpp_storage`, `xpp_reset_gates` and the GPIO banks.
    #[serde(default)]
    pub xpp_lens: Option<XppLens>,
    /// Shutter motor and position sensors on the GPIO banks.
    #[serde(default)]
    pub xpp_shutter: Option<XppShutter>,
    /// Keys on external interrupt lines: (line, bank, pin) and the poll period.
    #[serde(default)]
    pub xpp_key_lines: Option<XppKeyLines>,
    #[serde(default)]
    pub xpp_display_frames: Option<XppDisplayFrames>,
    #[serde(default)]
    pub xpp_adc: Option<XppAdc>,
    #[serde(default)]
    pub xpp_display_outputs: Vec<XppDisplayOutput>,
    /// X-Processor Pro clock bank: base, PLL lock ticks and running PLLs.
    #[serde(default)]
    pub xpp_clocks: Option<(u64, u64, Vec<usize>)>,
    /// X-Processor Pro imaging control page base.
    #[serde(default)]
    pub xpp_imaging_control: Option<u64>,
    /// X-Processor Pro reset and gate controls base.
    #[serde(default)]
    pub xpp_reset_gates: Option<u64>,
    #[serde(default)]
    pub xpp_sd: Option<XppSd>,
    /// Standard SDHCI hosts (registers from +0) with a FAT card each: the
    /// XP4 X-T3 card slots at 0xfffc4000 and 0xfffc8000. Card files go to the
    /// first host when no X-Processor Pro SD wrapper is declared.
    #[serde(default)]
    pub sd_hosts: Vec<SdHostInput>,
    /// X-Processor Pro DesignWare I2C buses with their declared parts.
    #[serde(default)]
    pub xpp_i2c: Vec<XppI2c>,
    /// JPEG encoder and decoder (the X-Processor Pro models): (base, line,
    /// run ticks). X-T3 5.11: decoder 0xfffa1000 on GIC 144 (driver
    /// 0x1093238 writes +0x118 = 1; handler 0x1090e24), encoder 0xfffa0000.
    /// Register pages whose job is acknowledged from another block's word
    /// (X-T3 5.11: playback engine 0xfa000000, GIC 145, acknowledge 0xfff4aa10 bit 3).
    #[serde(default)]
    pub job_ack: Vec<fujimod_emulator_xpp::job_ack::Options>,
    /// Mali-V61 style video unit (X-T3 5.11: 0xffe00000, GIC 138, queues
    /// at VPU address 0x10079000), opt-in.
    #[serde(default)]
    pub mali_v61: Option<fujimod_emulator_xpp::mali_v61::Options>,
    #[serde(default)]
    pub jpeg_encoder: Option<(u64, u32, u64)>,
    #[serde(default)]
    pub jpeg_decoder: Option<(u64, u32, u64)>,
    /// XP4 16-channel timer/PWM unit: (base, interrupt of channel 0) (opt-in;
    /// X-T3 `0xfff3c000`, GIC 41..56).
    #[serde(default)]
    pub timers16: Option<(u64, u32)>,
    /// ARM PL011 UARTs with nothing attached: (base, optional GIC line).
    /// X-T3 5.11: three ports at 0xfff39000 + 0x1000·n; its update program
    /// opens port 0.
    #[serde(default)]
    pub pl011: Vec<(u64, Option<u32>)>,
    /// XP4 F_I2C channels (opt-in; X-T3 battery gauge at 0x36 on channel 1).
    #[serde(default)]
    pub fi2c: Vec<Fi2c>,
    /// X-Processor Pro DRAM controller base and refresh-rate level.
    #[serde(default)]
    pub xpp_dram: Option<(u64, u8)>,
    /// Physical base of a writable alias of the whole DRAM.
    #[serde(default)]
    pub dram_alias: Option<u64>,
    /// X-Processor Pro DWC3: base, interrupt line, command and transaction
    /// ticks, negotiated speed (0 high speed, 4 SuperSpeed).
    #[serde(default)]
    pub xpp_usb: Option<(u64, u32, u64, u64, u8)>,
    /// X-Processor Pro power control base (supply hold and power domains).
    #[serde(default)]
    pub xpp_power: Option<u64>,
    /// X-Processor Pro imaging statistics engine: base, interrupt line and
    /// frame period in ticks.
    #[serde(default)]
    pub xpp_statistics: Option<(u64, u32, u64)>,
    /// Uniform scene for the statistics engine: the red, green and blue sum
    /// of every zone.
    #[serde(default)]
    pub xpp_statistics_scene: Option<[u32; 3]>,
    /// Statistics engine buffers written each frame: output register offset
    /// and words.
    #[serde(default)]
    pub xpp_statistics_buffers: Vec<(u64, Vec<u32>)>,
    /// Uniform live-view image: unit base, lines and Y, Cb, Cr.
    #[serde(default)]
    pub xpp_live_view_image: Option<(u64, u32, [u8; 3])>,
    /// X-Processor Pro face detection engine: base, interrupt line and run
    /// time in ticks.
    #[serde(default)]
    pub xpp_face: Option<(u64, u32, u64)>,
    /// X-Processor Pro register blocks with no work in flight.
    #[serde(default)]
    pub xpp_register_blocks: Vec<XppRegisterBlock>,
    /// DesignWare HDMI TX base; the board has no sink attached.
    #[serde(default)]
    pub xpp_hdmi: Option<u64>,
    /// X-Processor Pro imaging frame sync; its interrupt words lie inside
    /// the imaging control page.
    #[serde(default)]
    pub xpp_frame_sync: Option<XppFrameSync>,
    #[serde(default)]
    pub cache_sizes: Option<(u32, u32, u32)>,
    #[serde(default)]
    pub io_group_base: Option<u64>,
    #[serde(default)]
    pub decompressor: Option<(u64, u32)>,
    #[serde(default)]
    pub lcdc: Vec<(u64, u64, u32, u32, Option<String>, Option<String>)>,
    #[serde(default)]
    pub dmp: Option<(u64, u32)>,
    #[serde(default)]
    pub adc: Option<([u16; 12], u64)>,
    #[serde(default)]
    pub gpio: Option<[u16; 16]>,
    /// GPIO bank base; the XP4 0xfff41000 when absent.
    #[serde(default)]
    pub gpio_base: Option<u64>,
    /// External GPIO inputs wired to GIC lines: (port, pin, falling line,
    /// rising line), 0 = no line. An outside level change pulses the line of
    /// its edge (X-T3 5.11 lens ready 15.1: falling 182, rising 166).
    #[serde(default)]
    pub gpio_irqs: Vec<(usize, usize, u32, u32)>,
    /// Drive the `gpio_irqs` lines as GIC input levels instead of edge
    /// pulses: the rising line follows the pin, the falling line its
    /// inverse. A line the firmware configures level-sensitive then stays
    /// pending while the pin holds its active level and pends again after
    /// EOI (X-T3 5.11 lens ready 15.1 → 166 in mount mode 3). Off by default.
    #[serde(default)]
    pub gpio_irq_levels: bool,
    #[serde(default)]
    pub temperature: Option<(i32, u64)>,
    #[serde(default)]
    pub power_spi: Option<(u64, bool, usize, usize, bool, BTreeMap<u8, u16>, u64)>,
    /// X-T3 battery gauge sharing the power controller's SPI channel:
    /// (base, idle, chip-select port, pin, active high, read inputs by even
    /// command, writable even commands). The base must equal `power_spi`'s.
    #[serde(default)]
    pub gauge_spi: Option<(u64, bool, usize, usize, bool, BTreeMap<u8, u16>, Vec<u8>)>,
    #[serde(default)]
    pub checksum_power_spi: Option<(
        u64,
        bool,
        usize,
        usize,
        bool,
        BTreeMap<u8, u16>,
        Vec<(u8, u16)>,
        Option<fujimod_emulator_xp4_io::rtc::StagedConfig>,
        Vec<u8>,
    )>,
    /// Extra imaging line pulsed at each frame start: (cause mask in +0x140,
    /// GIC line). X-T3 5.11: (2, 101), the still capture's frame counter.
    #[serde(default)]
    pub imaging_start_pulse: Option<(u32, u32)>,
    /// Statistics SRAM bytes visible to the guest (opt-in, at least the two
    /// 0x7000 banks). X-T3 5.11 copies 0xd800 bytes from 0xfff17000 after a
    /// still capture, so its SRAM extends past 0xfff1e000.
    #[serde(default)]
    pub imaging_statistics_window: Option<u64>,
    /// Sensor read-out DMA lines of channels 0 and 1 (opt-in; default: the
    /// DMA words are only latched and no image moves). X-T3 5.11 programs
    /// channel 0 for a still and waits on source 0x4b = GIC 97 (channel 1:
    /// source 0x4c = GIC 98, table 0x13dc968).
    #[serde(default)]
    pub imaging_dma: Option<[u32; 2]>,
    /// Event bits of input-buffer enable word +0x04 that complete at the next
    /// frame edge, like the statistics bit 8 (opt-in, bits 0..7). X-T3 5.11
    /// still capture re-reads the RAW through 0xfff70000; its GIC 106 handler
    /// (0x104edf8, poll 0x104eb24) waits for an event bit 0..5 in +0x0c.
    #[serde(default)]
    pub imaging_input_events: Option<u32>,
    /// Duration of a still read-out in ticks, from the start of the frame
    /// that serves it (opt-in; default: the read-out ends with that frame).
    /// X-T3 5.11 still capture: GIC 97 (channel 0) and GIC 106 must follow the
    /// next VI fall; a full-sensor read-out takes several frames.
    #[serde(default)]
    pub imaging_still_readout_ticks: Option<u64>,
    /// Whole-frame still read-out (opt-in, exclusive with the ticks above): a
    /// still requested while a frame is under way is served by the next frame
    /// (X-T3 5.11 suite 36: the transfer cannot join a frame already started).
    #[serde(default)]
    pub imaging_dma_whole_frames: bool,
    /// Paint the read-out DMA from the run's scene (opt-in; default: the
    /// moving test ramp). The scene is the statistics scene of the input
    /// buffer, mosaiced through the X-T3 5.11 X-Trans table (unit 6,
    /// 0x22073f8) with phase (0, 0) at the first sample of each frame.
    #[serde(default)]
    pub imaging_raw_scene: bool,
    /// Write each read-out line at the pitch the firmware programs in the
    /// channel's +0x18 word (opt-in; default: lines packed). X-T3 5.11 reads
    /// the RAW back at that pitch (RECC +0x14 = 0x31e0).
    #[serde(default)]
    pub imaging_dma_line_pitch: bool,
    /// Time imaging frames from the sensor timing generator words of the
    /// command block (opt-in; default: the declared frame period), with the
    /// generator clock and the tick rate given explicitly. X-T3 5.11:
    /// {"clock_hz": 72000000, "tick_hz": 54000000} (generator clock from table
    /// 0x22aca90 code 2; 54 MHz timer); 4K 23.98p programs 3 000 000 clocks a
    /// frame = 2 250 000 ticks. A stopped generator (+0x184 bit 0 clear) keeps
    /// the declared period. Only the imaging receiver follows it.
    #[serde(default)]
    pub imaging_tg_cadence: Option<fujimod_emulator_sensor_input::sensor::TgCadence>,
    /// Withhold the first `frames` frame periods after each timing generator
    /// start (+0x184 bit 0 from 0 to 1; opt-in; default: none): no frame
    /// start or end, cause, IRQ or read-out DMA in those periods, and the
    /// frame counter skips them; the cadence goes on. Models a sensor that
    /// needs a few vertical periods before it streams again (X-T3 5.11: the
    /// STB3 -> ACT restart of 0x20c342c). Needs `imaging_tg_cadence`. The plan
    /// action `imaging_tg_restart_delay` changes it between slices.
    #[serde(default)]
    pub imaging_tg_restart_delay: Option<fujimod_emulator_sensor_input::sensor::TgRestartDelay>,
    /// Scale the RAW scene and the input-buffer statistics by the analog gain
    /// the firmware programs over SPI (opt-in; default: the scene as is).
    /// Lane 0 code = register 0x30 | (0x31 & 15) << 8, linear gain
    /// 4095 / (4095 - code), capped at 0xefc (X-T3 5.11 0x20c2c88), divided by
    /// the gain of `gain_ref`, the code the body sends at base sensitivity. A
    /// code never written scales by 1. `conversion_gain` (optional) multiplies
    /// while register 0x2f bit 0 is set (high conversion gain; about 3 from
    /// T44-ex160 / ex640). `shutter` (optional, default absent = gain only)
    /// also scales by the lines of integration the firmware programs:
    /// (n + 1) * VMAX + offset_eighths / 8 - 2 * SHS (registers 0x21 / 0x22,
    /// 0x25 / 0x26, timing generator word +0x194; `free_run_lines` replaces a
    /// VMAX equal to `free_run_vmax`), divided by `ref_lines`; see
    /// `SensorShutter`. Needs `imaging_raw_scene`.
    #[serde(default)]
    pub imaging_sensor_exposure: Option<fujimod_emulator_sensor_input::sensor::SensorExposure>,
    /// Fill the X-T3 5.11 statistics outputs beyond the 0x254 colour sums
    /// (opt-in; default: only 0x254). On each frame that serves enable bit
    /// 0x100, the input buffer also writes, at the destinations programmed in
    /// its registers 0x244 (zone luminance grid), 0x25c (ten window records),
    /// 0x2b4 (256-class histogram) and 0x274 / 0x27c / 0x284 (optical-black
    /// sums), values computed from the statistics scene and the followed
    /// sensor gain. No completion bit is added. `{}` takes every default.
    #[serde(default)]
    pub imaging_statistics_outputs:
        Option<fujimod_emulator_sensor_input::statistics::StatisticsOutputs>,
    /// Keep the frame start pulse while the programmed sensor is in standby
    /// (register 0 bit 0 set; opt-in; default: a sensor in standby stops the
    /// frame timing). The receiver goes on timing frames, receiver links
    /// stopped or not, and pulses the start line only: no timing IRQ and no
    /// frame data (no receiver cause, read-out DMA or input-buffer frame; the
    /// frame counter does not move). With `imaging_tg_cadence`, a stopped
    /// generator still stops everything. X-T3 5.11: ccdtgSetSleepModeForMV
    /// (0x20c37a8) writes 0x1ee |= 4 then register 0 |= 1 after the 0x8d
    /// restart, stops link 0 (+0 <- 0x10001) and wakes the sensor only after
    /// counting start pulses (slot 2, GIC 101, 0x20986ac -> 0x20a1c24); the
    /// end-of-frame timing line (slot 0, GIC 100, 0x209c94c -> 0x2097f9c)
    /// re-arms that count, so it stays quiet in standby.
    #[serde(default)]
    pub imaging_standby_timing: bool,
    #[serde(default)]
    pub imaging: Option<(
        (u64, u64, Vec<u32>, Vec<u32>, Vec<u32>),
        (u64, u64, u64, u64),
        (u16, u8),
        Option<(u64, u32, u64, u64, u64, String)>,
    )>,
    #[serde(default)]
    pub movdc: Vec<(u64, u32, u64, u64, u8)>,
    /// Fill every MOVDC output with the statistics scene instead of its
    /// declared fixed luma (opt-in; default: the fixed luma). Each frame is
    /// the scene stretched to the programmed output size, with the followed
    /// sensor gain, as packed Y0,Y1,Cb,Cr (law: `ViewPicture`, not an ISP
    /// model). Needs `movdc` and the imaging block. `{}` takes the defaults.
    #[serde(default)]
    pub movdc_scene: Option<fujimod_emulator_sensor_input::statistics::ViewPicture>,
    /// Accept MOVDC output format 1 (+08 = 0x101, X-T3 5.11 FHD movie
    /// recording) as 4:2:0: only the luma plane is written, from the scene
    /// when `movdc_scene` is set, else the fixed luma (opt-in; default: the
    /// format is refused). Needs `movdc`.
    #[serde(default)]
    pub movdc_yuv420: bool,
    #[serde(default)]
    pub sd_clocks: Vec<(u64, u64)>,
    #[serde(default = "instruction_rate")]
    pub instructions_per_tick: (u64, u64),
    /// Advance guest time by `instructions_per_tick` per core: each
    /// instruction counts for 1/n of its share while n cores are runnable, so
    /// a round of n cores in parallel moves time as one core would.
    #[serde(default)]
    pub per_core_time: bool,
    #[serde(default)]
    pub usb: Option<(u64, u32, u64, u64, u8)>,
    #[serde(default)]
    pub usb_phy: Option<(u64, [u8; 32], u32)>,
    #[serde(default)]
    pub xpp_boot: Option<XppBoot>,
    #[serde(default)]
    pub pmu_cycles_per_tick: Option<u64>,
    /// Declared (port base, send word, receive word) replies.
    #[serde(default)]
    pub xpp_serial_replies: Vec<(u64, u32, u32)>,
}

/// Register layout of the interval timer at `timer_base`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerLayout {
    /// Eight channels; select bits set in the control word.
    Xp4,
    /// Sixteen channels; select bits clear in the control word.
    Xpp,
}

/// Byte-frame peer on one X-Processor Pro serial port: (even register,
/// 16-bit value) pairs after reset; unlisted registers read zero.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppBatteryGauge {
    pub port: u64,
    pub registers: Vec<(u8, u16)>,
}

/// Register-file peer on one X-Processor Pro serial port: (register, value)
/// pairs after reset; unlisted registers read zero.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppSubCpu {
    pub port: u64,
    pub registers: Vec<(u8, u8)>,
    /// Seconds count kept in the register file.
    #[serde(default)]
    pub clock: Option<fujimod_emulator_xpp::serial::Clock>,
}

/// X-mount lens: serial port base, information page and command replies,
/// the body's power output (bank, register, bit), the ready pin (bank, pin)
/// and its external interrupt line, the contact levels (bank, pin, high) and
/// the declared delays in ticks.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppLens {
    pub port: u64,
    pub page: Vec<u8>,
    pub replies: Vec<(u32, u16)>,
    pub power: (usize, usize, usize),
    pub ready: (usize, usize),
    pub line: usize,
    pub contacts: Vec<(usize, usize, bool)>,
    pub ready_ticks: u64,
    pub release_ticks: u64,
    pub drive_ticks: u64,
    pub poll_ticks: u64,
}

/// Shutter: the motor pins' mode register (bank, register) and pins, the
/// sensor inputs (bank, pin), their external interrupt line, the sensor
/// levels in the order the motor reaches them and the declared ticks.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppShutter {
    pub mode: (usize, usize),
    pub motor: (usize, usize),
    pub sensors: Vec<(usize, usize)>,
    pub line: usize,
    pub positions: Vec<Vec<bool>>,
    pub step_ticks: u64,
    pub poll_ticks: u64,
}

/// Keys whose GPIO inputs also reach external interrupt lines.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppKeyLines {
    pub lines: Vec<(usize, usize, usize)>,
    pub poll_ticks: u64,
}

/// X-Processor Pro display frame interrupts: one edge per line each period.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppDisplayFrames {
    #[serde(default)]
    pub timing: Option<([u64; 2], u64)>,
    pub irqs: Vec<u32>,
    pub period_ticks: u64,
}

/// X-Processor Pro display output registers: word offsets whose status bits
/// read set or clear regardless of the last write, and the optional
/// (host output, capture period in ticks) of its OSD layer scanout.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppDisplayOutput {
    #[serde(default)]
    pub handshake_ticks: Option<u64>,
    pub base: u64,
    pub size: u64,
    pub set_bits: Vec<(u64, u32)>,
    pub clear_bits: Vec<(u64, u32)>,
    #[serde(default)]
    pub scanout: Option<(usize, u64)>,
}

/// X-Processor Pro SD channel with a freshly formatted card in the slot.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppSd {
    pub options: fujimod_emulator_xpp::sd::Options,
    pub card: fujimod_emulator_storage::fat::Options,
    #[serde(default)]
    pub irq: Option<u32>,
    /// SD channel of the reset-gate line; requires `xpp_reset_gates`.
    #[serde(default)]
    pub reset_channel: Option<usize>,
}

/// A standard SDHCI host, its card and its optional interrupt line (polled
/// when absent: an enabled interrupt assertion then refuses).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdHostInput {
    pub options: fujimod_emulator_ip::sdhci::host::Options,
    pub card: fujimod_emulator_storage::fat::Options,
    #[serde(default)]
    pub irq: Option<u32>,
}

/// X-Processor Pro imaging frame sync: register base, the address of the
/// frame interrupt words, the (source, line) pairs frames latch and the frame
/// period in ticks.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppFrameSync {
    pub base: u64,
    pub interrupts: u64,
    pub frame_lines: Vec<(usize, u32)>,
    pub period_ticks: u64,
}

/// X-Processor Pro register block: base, window size, the (offset, mask)
/// status bits that always read set and an optional frame or job law.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppRegisterBlock {
    pub base: u64,
    pub size: u64,
    pub idle: Vec<(u64, u32)>,
    #[serde(default)]
    pub frames: Option<XppBlockFrames>,
    #[serde(default)]
    pub job: Option<XppBlockJob>,
    /// (offset, value) of words that read a fixed value.
    #[serde(default)]
    pub fixed: Vec<(u64, u32)>,
    /// Commands that set a field of a status word.
    #[serde(default)]
    pub commands: Vec<XppBlockCommand>,
    /// X-T3 5.11 RECC 0xfff82000 develops the still (opt-in, needs the job).
    #[serde(default)]
    pub recc: Option<XppRecc>,
    /// Report the block under its own name, `xpp-register-block@<base>`
    /// (opt-in; default: the name all plain blocks share).
    #[serde(default)]
    pub own_name: bool,
    /// X-T3 5.11 audio format converter 0xfffa8000 converts each job's
    /// samples (opt-in, needs the job; see
    /// [`fujimod_emulator_xpp::blocks::AUDIO_CONTROL`]). Off, the job only
    /// raises its interrupt and movie sound plays as silence.
    #[serde(default)]
    pub audio_converter: bool,
}

/// Declared RECC development: RAW (address, pitch in bytes) and X-Trans
/// site (line, sample) of its first sample, and the area (address, length in
/// bytes) RECC may write.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppRecc {
    pub raw: (u64, u64),
    pub site: (u8, u8),
    pub area: (u64, u64),
    /// Also model the 4K movie mode +0x08 = 0x422000 (opt-in; X-T3 5.11 REC
    /// 4K, service 0x1293), see
    /// [`fujimod_emulator_xpp::blocks::ReccMovie`].
    #[serde(default)]
    pub movie: Option<XppReccMovie>,
    /// Develop with the firmware color of these declared units (opt-in;
    /// default: the declared sRGB).
    #[serde(default)]
    pub firmware_color: Option<XppReccColor>,
}

/// RECC's declared 4K movie picture: the right shift from a RAW sample to
/// 8-bit luma, and whether to develop in color (the row layout is read from
/// the output code the firmware programs).
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppReccMovie {
    pub shift: u32,
    #[serde(default)]
    pub color: bool,
}

/// Bases of the register blocks whose words give RECC's firmware color: the
/// tone unit (X-T3 5.11 0xfff86000: matrix, curves) and the YCbCr unit
/// (0xfff87000). Both must be declared register blocks.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppReccColor {
    pub tone: u64,
    pub ycbcr: u64,
    /// Also honour the chroma mode fields of the YCbCr unit's control word
    /// (opt-in, see [`fujimod_emulator_xpp::development::FirmwareColor`]).
    #[serde(default)]
    pub chroma_modes: bool,
    /// Also apply the YCbCr unit's 2x2 chroma matrix (+0x238, +0x23c;
    /// opt-in, see
    /// [`fujimod_emulator_xpp::development::FirmwareColor::with_chroma_matrix`]).
    #[serde(default)]
    pub chroma_matrix: bool,
    /// Also apply the tone unit's brightness gain (+0x004 weights, +0x008
    /// table; opt-in, see
    /// [`fujimod_emulator_xpp::development::FirmwareColor::with_brightness_gain`]).
    #[serde(default)]
    pub brightness_gain: bool,
    /// Also apply the YCbCr unit's luma bands (+0x258..+0x26c; opt-in, see
    /// [`fujimod_emulator_xpp::development::FirmwareColor::with_luma_bands`]).
    #[serde(default)]
    pub luma_bands: bool,
    /// Also apply the tone unit's white balance (+0x088/+0x08c gains),
    /// relative to these declared reference (R, G, B) gains at which the
    /// declared scene renders neutral (opt-in; X-T3 5.11 auto white balance
    /// of the declared daylight scene: 9149, 4822, 9309; see
    /// [`fujimod_emulator_xpp::development::FirmwareColor::with_white_balance`]).
    #[serde(default)]
    pub white_balance: Option<[u32; 3]>,
    /// Also apply the 3D lookup table of the LUT unit (opt-in, see
    /// [`fujimod_emulator_xpp::development::FirmwareColor::with_lut3d`]).
    #[serde(default)]
    pub lut3d: Option<XppReccLut3d>,
}

/// Bases of the LUT unit's blocks (X-T3 5.11: control 0xfff98000, table
/// 0xfff90000): the control block and the first of the eight consecutive
/// 0x1000-byte blocks that hold the table. All must be declared register
/// blocks.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppReccLut3d {
    pub control: u64,
    pub table: u64,
}

impl XppReccLut3d {
    /// Bases of the eight table blocks.
    pub fn tables(&self) -> [u64; 8] {
        std::array::from_fn(|k| self.table + 0x1000 * k as u64)
    }
}

/// Command law of a register block: writing exactly `value` to `word` sets
/// the `mask` field of the `status` word to `state`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppBlockCommand {
    pub word: u64,
    pub value: u32,
    pub status: u64,
    pub mask: u32,
    pub state: u32,
}

/// Job law of a register block: (offset, mask) of the start bit, of the
/// status bit (none: the job pulses its interrupt) and of a stop bit, the
/// level interrupt line and the ticks a job takes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppBlockJob {
    pub start: (u64, u32),
    #[serde(default)]
    pub status: Option<(u64, u32)>,
    #[serde(default)]
    pub stop: Option<(u64, u32)>,
    pub irq: u32,
    pub ticks: u64,
    /// A status job's start bit reads clear again once the job ends
    /// (X-T3 5.11 IIFP 0xfff6f000).
    #[serde(default)]
    pub start_clears: bool,
}

/// Frame law of a register block: (offset, mask) of the start and stopped
/// bits, the interrupt line and the frame period in ticks.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppBlockFrames {
    pub start: (u64, u32),
    pub stopped: (u64, u32),
    pub irq: u32,
    pub period_ticks: u64,
}

/// One XP4 Socionext F_I2C channel (X-T3 5.11: 0xfff3d000 + 0x1000·n) and
/// the parts that answer on it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fi2c {
    pub options: fujimod_emulator_ip::socionext::fi2c::Options,
    pub parts: Vec<fujimod_emulator_xpp::i2c::Part>,
}

/// One X-Processor Pro DesignWare I2C bus and the parts that answer on it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppI2c {
    pub options: fujimod_emulator_ip::dw_i2c::Options,
    pub parts: Vec<fujimod_emulator_xpp::i2c::Part>,
}

/// X-Processor Pro ADC: register base, twelve 10-bit samples and the ticks a
/// single conversion takes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppAdc {
    pub base: u64,
    pub samples: Vec<u16>,
    pub conversion_ticks: u64,
}

/// X-Processor Pro flash controller, read aperture and PL330 wiring.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppStorage {
    pub flash_controller: u64,
    pub aperture: u64,
    pub aperture_size: u64,
    pub dma: u64,
    pub dma_irq_routes: Vec<u32>,
    pub dma_setup_ticks: u64,
    pub dma_bytes_per_tick: u64,
    /// Interrupt line of the controller status.
    #[serde(default)]
    pub irq: Option<u32>,
    /// NOR device attached to one chip select, with its array timing.
    #[serde(default)]
    pub nor: Option<XppNor>,
}

/// Declared NOR register device and array behind one X-Processor Pro chip select.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppNor {
    pub chip_select: u8,
    pub registers: fujimod_emulator_devices::parts::micron_nor::Options,
    pub array: fujimod_emulator_xpp::flash::Array,
}

/// X-Processor Pro cold-reset controller and its boot-selection strap level.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XppBoot {
    pub options: fujimod_emulator_xpp::boot::Options,
    pub card_strap: bool,
    /// Memory window shown at physical 0 until the guest selects the remap.
    #[serde(default)]
    pub low_alias: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageDmaLens {
    pub fifo: u64,
    pub link: fujimod_emulator_xp4_storage::lens_peer::Configuration,
    /// The lens's ready line; absent, nothing drives it (the default).
    #[serde(default)]
    pub motion_end: Option<LensMotionEnd>,
}
/// Lens ready line (X-T3 5.11: pin 15.1, lens power output 15.0). When the
/// body drives the power pin as a high output, the lens pulls the ready pin
/// low after `ready_ticks` and lets it rise `release_ticks` later (start-up
/// done). From then on, each release of the ready pin by the body (a
/// register-1 write clearing it) holds the pin low for `drive_ticks` (a
/// movement). Releases during start-up or without power are not answered.
/// The lens samples both pins every `poll_ticks`. Declared delays, not
/// measurements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct LensMotionEnd {
    pub pin: (usize, usize),
    pub power: (usize, usize),
    pub ready_ticks: u64,
    pub release_ticks: u64,
    pub drive_ticks: u64,
    pub poll_ticks: u64,
}
