#![no_std]
#![no_main]

extern crate alloc;

#[cfg(not(any(feature = "board-l431", feature = "board-bu03")))]
compile_error!("select exactly one board feature: `board-l431` or `board-bu03`");
#[cfg(all(feature = "board-l431", feature = "board-bu03"))]
compile_error!("`board-l431` and `board-bu03` are mutually exclusive");
#[cfg(all(feature = "base-station", feature = "sender"))]
compile_error!(
    "`base-station` and `sender` are mutually exclusive; pick at most one (`sender` is the default)"
);

use alloc::sync::Arc;

use defmt::*;
use defmt_rtt as _;
use dw3000_ng::hl::SendTime;
use dw3000_ng::time::Instant as DwInstant;
use dw3000_ng::{self, Config as DwConfig, DW3000, Ready, Sending, SingleBufferReceiving};
use embassy_executor::Spawner;
use embassy_stm32::dma;
use embassy_stm32::gpio::{Level, Output, Pin, Speed};
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::mode::Async;
use embassy_stm32::peripherals::SPI1;
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::wdg::IndependentWatchdog;
use embassy_stm32::{Config, Peri, Peripherals, bind_interrupts, peripherals};
use embassy_time::{Duration, Timer};
use embedded_alloc::LlffHeap as Heap;
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use panic_probe as _;
use smoltcp::wire::{
    Ieee802154Address, Ieee802154Frame, Ieee802154FrameType, Ieee802154FrameVersion, Ieee802154Pan,
    Ieee802154Repr,
};
use spin::Mutex;

mod device;
mod interfaces;
mod prng;

use device::Device;
use interfaces::hardware_interface::{HardwareInterface, IncomingMessage, UwbAddress};
use interfaces::hardware_interface_real_impl::RealHardwareInterface;

bind_interrupts!(struct Irqs {
    DMA1_CHANNEL2 => dma::InterruptHandler<peripherals::DMA1_CH2>;
    DMA1_CHANNEL3 => dma::InterruptHandler<peripherals::DMA1_CH3>;
});

// ---------------------------------------------------------------------------
// Global allocator
// ---------------------------------------------------------------------------

// The mesh's core routing/OGM-age tables are fixed-capacity arrays (see
// `device::array_map::ArrayMap`) and don't need the heap; this only has to
// cover `Arc`/`spin::Mutex` bookkeeping plus `Vec<u8>`/`String` message
// buffers in `device::{mesh,firmware}`. Starting sizes below are a
// conservative guess, not derived from a real budget analysis - tune if
// allocation failures show up in practice.
#[global_allocator]
static HEAP: Heap = Heap::empty();

#[cfg(feature = "board-l431")]
const HEAP_SIZE: usize = 8192;
#[cfg(feature = "board-bu03")]
const HEAP_SIZE: usize = 2048;

// ---------------------------------------------------------------------------
// Crash diagnostics
// ---------------------------------------------------------------------------

/// Intercept every hard fault (including the `udf` that `panic_probe` triggers
/// after printing a panic message). Logs the faulting PC/LR so we can see where
/// the device died, lets the host drain the RTT buffer, then resets instead of
/// hanging silently.
#[cortex_m_rt::exception]
unsafe fn HardFault(frame: &cortex_m_rt::ExceptionFrame) -> ! {
    error!(
        "HardFault: pc={=u32:x} lr={=u32:x} xpsr={=u32:x}",
        frame.pc(),
        frame.lr(),
        frame.xpsr()
    );
    defmt::flush();
    // ~14 ms @ 72 MHz: give probe-rs/J-Link time to read the RTT buffer out
    // before we reset (if the IWDG fires during this delay, that reset works
    // just as well).
    cortex_m::asm::delay(1_000_000);
    cortex_m::peripheral::SCB::sys_reset();
}

// ---------------------------------------------------------------------------
// Roles and addressing
// ---------------------------------------------------------------------------

/// Node role, selected at compile time:
/// - `base-station` (`--features base-station`): broadcasts an OGM once a
///   minute so senders learn (and refresh) the route to it.
/// - `sender` (default): sends a `ping` unicast to every routing-table entry
///   once every 10 seconds.
///
/// Every node follows the same sleep/wake duty cycle (see `run_mesh`): it
/// receives and transmits only inside its wake window and puts the DW3000
/// into true DEEPSLEEP in between, during which nothing is received.

/// PAN ID used by all boards.
const PAN_ID: u16 = 0x0001;
/// Short address of the base-station board.
#[cfg(feature = "base-station")]
const BASE_STATION_ADDRESS: [u8; 2] = [0xAA, 0x01];
/// Short address of the sender board.
#[cfg(not(feature = "base-station"))]
const SENDER_ADDRESS: [u8; 2] = [0xAA, 0x02];

/// IWDG watchdog timeout. Must comfortably exceed the longest gap between
/// `pet()` calls: the sleep phase pets every 500 ms, everything else iterates
/// far faster. Short enough that a genuinely hung SPI transaction or
/// stalled loop still auto-recovers in a few seconds.
const WATCHDOG_TIMEOUT_US: u32 = 3_500_000;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    unsafe {
        embedded_alloc::init!(HEAP, HEAP_SIZE);
    }

    let mut config = Config::default();
    board_rcc_config(&mut config);
    let p = embassy_stm32::init(config);

    Timer::after(Duration::from_millis(2000)).await;

    info!("Hello World!");

    // Diagnose why the previous boot ended: an IWDG reset flag set means the
    // watchdog had to recover the device (a freeze/panic we did not catch).
    {
        use embassy_stm32::pac::RCC;
        let csr = RCC.csr().read();
        if csr.iwdgrstf() {
            warn!("Reset cause: IWDG timeout (previous boot was reset by the watchdog)");
            RCC.csr().modify(|w| w.set_rmvf(true));
        } else if csr.sftrstf() {
            warn!("Reset cause: software/system reset");
        }
    }

    // Independent watchdog: if the firmware ever hangs (stuck SPI transaction,
    // dead executor, unhandled fault), the device resets itself after
    // WATCHDOG_TIMEOUT_US instead of freezing forever. Fed from the main loops.
    let mut wdg =
        unsafe { IndependentWatchdog::new(p.IWDG.clone_unchecked(), WATCHDOG_TIMEOUT_US) };
    wdg.unleash();
    info!(
        "IWDG watchdog armed ({} ms timeout)",
        WATCHDOG_TIMEOUT_US / 1000
    );

    // Own short address follows the node role so the boards get distinct
    // addresses out of the box.
    #[cfg(feature = "base-station")]
    let short_address = BASE_STATION_ADDRESS;
    #[cfg(not(feature = "base-station"))]
    let short_address = SENDER_ADDRESS;
    info!(
        "mesh node booting, short={:02x}:{:02x}",
        short_address[0], short_address[1]
    );
    let dw = build_dw(&p, short_address).await;
    run_mesh(p, dw, wdg, short_address).await;
}

// ---------------------------------------------------------------------------
// Mesh on real hardware
//
// The mesh (`device::Device`) is synchronous: `heartbeat()` expects a sync
// `HardwareInterface`. The DW3000 driver is async and owned by this async
// task, so `RealHardwareInterface` is a pure queue bridge (see its docs):
// `send_message` enqueues, the pump below transmits; received frames are
// decoded and pushed back for `get_incoming_message`.
//
// Every node follows a sleep/wake duty cycle: inside the wake window it
// pumps RX, heartbeats the mesh and drains TX at full rate; then it puts the
// DW3000 into true DEEPSLEEP. Nothing is received while asleep (pump and
// heartbeat stop, the chip is off); wake-up is a full hardware reset and
// re-init via `build_dw`.
// ---------------------------------------------------------------------------

/// How long the receiver stays in RX per poll iteration.
const MESH_RX_WINDOW: Duration = Duration::from_millis(8);

/// Base-station: OGM broadcast interval.
#[cfg(feature = "base-station")]
const BASE_STATION_OGM_INTERVAL: Duration = Duration::from_secs(10);
/// Sender: ping interval.
#[cfg(not(feature = "base-station"))]
const SENDER_PING_INTERVAL: Duration = Duration::from_secs(10);

/// Application-layer ping/ack protocol, carried as opaque unicast payloads
/// *through* the mesh (the mesh itself doesn't know about it). Payload:
///   ping: [APP_MSG_PING][counter: u32 BE]
///   ack:  [APP_MSG_ACK ][counter: u32 BE]
/// The counter identifies the ping, so the sender only stops retransmitting
/// once the destination acknowledges that specific ping.
const APP_MSG_PING: u8 = 0x01;
// const APP_MSG_ACK: u8 = 0x02; // TODO: remove this

/// Decodes an application-layer message into its (type, counter), or `None`
/// if the payload isn't a well-formed 5-byte app message.
fn decode_app_message(data: &[u8]) -> Option<(u8, u32)> {
    if data.len() == 5 {
        Some((data[0], u32::from_be_bytes(data[1..5].try_into().ok()?)))
    } else {
        None
    }
}

/// Base-station: drain every addressed-to-me payload the mesh delivered and
/// reply an acknowledgement for each ping. Acks are flooded: every call
/// re-queues the ack for the newest ping seen, so the sender (which only
/// listens briefly between retransmissions) catches one no matter the phase
/// relationship between the two duty cycles. Acks are re-queued through the
/// mesh unicast path; the caller drains TX afterwards.
#[cfg(feature = "base-station")]
fn app_base_tick(device: &Device, pending_ack: &mut Option<(u32, UwbAddress)>) {
    while let Some((source, data)) = device.take_received_data() {
        if let Some((msg_type, counter)) = decode_app_message(&data) {
            if msg_type == APP_MSG_PING {
                let is_new = matches!(pending_ack, Some((c, _)) if *c == counter);
                if !is_new {
                    info!(
                        "base-station: ping #{} from {} -> flooding ack",
                        counter,
                        defmt::Debug2Format(&source)
                    );
                }
                *pending_ack = Some((counter, source));
            }
        }
    }
}

/// Builds the queue-bridge `HardwareInterface`, creates a `Device` running
/// the mesh on top of it, and drives everything: the sleep/wake duty cycle,
/// radio pumps, `heartbeat()`, and the role behavior (base-station OGMs /
/// sender pings).
async fn run_mesh(
    p: Peripherals,
    mut dw: DwReady,
    mut wdg: IndependentWatchdog<'static, peripherals::IWDG>,
    short_address: [u8; 2],
) -> ! {
    let seed = embassy_time::Instant::now().as_ticks() as u32;
    let hw: Arc<Mutex<dyn HardwareInterface>> =
        Arc::new(Mutex::new(RealHardwareInterface::new(short_address, seed)));

    let device_id = u16::from_be_bytes(short_address) as u64;
    let device = Device::create(device_id);
    device.init("mesh-node", "uwb-mesh-real-hw", hw.clone());

    let mut tx_seq: u8 = 0;

    #[cfg(feature = "base-station")]
    let mut last_ogm = embassy_time::Instant::now();
    #[cfg(not(feature = "base-station"))]
    let mut last_ping = embassy_time::Instant::now();
    #[cfg(not(feature = "base-station"))]
    let mut ping_counter: u32 = 0;
    #[cfg(not(feature = "base-station"))]
    let own_address = RealHardwareInterface::map_source(short_address);
    #[cfg(feature = "base-station")]
    let mut pending_ack: Option<(u32, UwbAddress)> = None;

    #[cfg(feature = "base-station")]
    {
        info!("mesh base-station running");
        // Advertise immediately so senders learn the route without waiting
        // a full flood interval (10 s) after boot.
        dw = send_ogm(
            &device,
            dw,
            hw.clone(),
            short_address,
            &mut tx_seq,
            &mut wdg,
            &mut pending_ack,
        )
        .await;
    }
    #[cfg(not(feature = "base-station"))]
    info!("mesh sender running");

    loop {
        wdg.pet();
        let (d, n) = poll_rx_window(dw, hw.clone()).await;
        dw = d;
        if n > 0 {
            info!("mesh: {} frame(s) received", n);
        }
        device.heartbeat();
        #[cfg(feature = "base-station")]
        app_base_tick(&device, &mut pending_ack);
        #[cfg(not(feature = "base-station"))]
        while device.take_received_data().is_some() {}
        let (d, n) = drain_tx(dw, hw.clone(), short_address, &mut tx_seq).await;
        dw = d;
        if n > 0 {
            info!("mesh: {} frame(s) sent", n);
        }

        #[cfg(feature = "base-station")]
        if embassy_time::Instant::now() - last_ogm >= BASE_STATION_OGM_INTERVAL {
            last_ogm = embassy_time::Instant::now();
            dw = send_ogm(
                &device,
                dw,
                hw.clone(),
                short_address,
                &mut tx_seq,
                &mut wdg,
                &mut pending_ack,
            )
            .await;
        }

        #[cfg(not(feature = "base-station"))]
        if embassy_time::Instant::now() - last_ping >= SENDER_PING_INTERVAL {
            last_ping = embassy_time::Instant::now();
            ping_counter = ping_counter.wrapping_add(1);
            // Application-layer ping: one logical message, re-sent as the
            // same 5-byte payload until the base-station acknowledges it.
            let mut payload_bytes = alloc::vec![APP_MSG_PING];
            payload_bytes.extend_from_slice(&ping_counter.to_be_bytes());
            // Ping every known destination; in this setup that is the
            // base-station learned from its OGMs. Each copy is re-sent
            // for longer than the base-station's sleep period so that at
            // least one hits its wake window.
            let routing_table = device.get_routing_table();
            let mut sent_to = 0u32;
            for (destination, _next_hop) in routing_table.iter() {
                if *destination == own_address {
                    continue;
                }
                sent_to += 1;
            }
            info!(
                "sender: ping #{} burst to {} route(s)",
                ping_counter, sent_to
            );
            for (destination, _next_hop) in routing_table.iter() {
                if *destination == own_address {
                    continue;
                }
                {
                    wdg.pet();
                    device.send_unicast(*destination, payload_bytes.clone());
                    device.heartbeat();
                    let (d, _) = drain_tx(dw, hw.clone(), short_address, &mut tx_seq).await;
                    dw = d;
                }
                info!(
                    "sender: ping #{} -> {} {}",
                    ping_counter,
                    defmt::Debug2Format(&destination),
                    "SENT"
                );
            }
        }
    }
}

/// Base-station: flood one OGM (single sequence number, `MESH_BURST_REPEATS`
/// copies, one queued per `MESH_BURST_STEP`) so the flood spans longer than
/// a neighbouring node's 2 s DEEPSLEEP period — a waking (but otherwise
/// phase-locked) node always catches a copy and learns the route to us.
/// Stays awake (no sleep) for the whole flood; returns the radio back.
#[cfg(feature = "base-station")]
async fn send_ogm(
    device: &Device,
    mut dw: DwReady,
    hw: Arc<Mutex<dyn HardwareInterface>>,
    short_address: [u8; 2],
    tx_seq: &mut u8,
    wdg: &mut IndependentWatchdog<'static, peripherals::IWDG>,
    pending_ack: &mut Option<(u32, UwbAddress)>,
) -> DwReady {
    info!("base-station: OGM flood starting");
    {
        wdg.pet();
        device.send_ogm();
        device.heartbeat();
        // app_base_tick(device, pending_ack);
        // let (d, _) = poll_rx_window_short(dw, hw.clone()).await;
        // dw = d;
        // let (d, _) = drain_tx(dw, hw.clone(), short_address, tx_seq).await;
        // dw = d;
    }
    info!("base-station: OGM flood done");
    dw
}

/// Listen for up to `MESH_RX_WINDOW`, decode every received 802.15.4 data
/// frame, and push the payloads into the hardware interface RX queue.
/// Returns the radio back plus the number of frames queued.
///
/// The DW3000 single-buffer receiver holds at most one frame per `receive()`
/// session, so the session is re-armed after every frame (or error) until
/// the window expires.
async fn poll_rx_window(mut dw: DwReady, hw: Arc<Mutex<dyn HardwareInterface>>) -> (DwReady, u32) {
    let cfg = dw_rf_config();
    let deadline = embassy_time::Instant::now() + MESH_RX_WINDOW;
    let mut buf = [0u8; 128];
    let mut count = 0u32;
    loop {
        let mut receiving = match dw.receive(cfg).await {
            Ok(r) => r,
            Err(e) => {
                error!("mesh rx: receive failed: {}", defmt::Debug2Format(&e));
                core::panic!("mesh rx: receive failed");
            }
        };
        // Poll this session until the first frame (or terminal error); then
        // finish it and re-arm with a fresh session so no frame is missed.
        let window_over = loop {
            match receiving.r_wait(&mut buf).await {
                Ok(msg) => {
                    if let Some(incoming) = decode_mesh_frame(&msg) {
                        hw.lock().push_incoming(incoming);
                        count += 1;
                    }
                    break false;
                }
                Err(nb::Error::WouldBlock) => {
                    if embassy_time::Instant::now() >= deadline {
                        break true;
                    }
                    Timer::after_micros(100).await;
                }
                Err(nb::Error::Other(e)) => {
                    if !is_retryable_rx_error(&e) {
                        error!(
                            "mesh rx: non-retryable radio error: {}",
                            defmt::Debug2Format(&e)
                        );
                        core::panic!("mesh rx: unexpected radio error");
                    }
                    // Session spent; re-arm unless the window expired.
                    break embassy_time::Instant::now() >= deadline;
                }
            }
        };
        dw = finish_receiving_or_panic(receiving).await;
        if window_over || embassy_time::Instant::now() >= deadline {
            break;
        }
    }
    (dw, count)
}

/// Finish an RX session, retrying once; panics (→ watchdog reset) if the
/// radio is wedged.
async fn finish_receiving_or_panic(receiving: DwReceiving) -> DwReady {
    match receiving.finish_receiving().await {
        Ok(d) => d,
        Err((r, e)) => {
            error!(
                "mesh rx: finish_receiving failed: {}",
                defmt::Debug2Format(&e)
            );
            match r.finish_receiving().await {
                Ok(d) => d,
                Err(_) => core::panic!("mesh rx: finish_receiving failed twice"),
            }
        }
    }
}

/// Short RX poll used inside burst loops (the surrounding loop already
/// sets the ~20 ms cadence, so a full window would slow the burst).
async fn poll_rx_window_short(
    dw: DwReady,
    hw: Arc<Mutex<dyn HardwareInterface>>,
) -> (DwReady, u32) {
    let cfg = dw_rf_config();
    let mut receiving = match dw.receive(cfg).await {
        Ok(r) => r,
        Err(e) => {
            error!("mesh rx: receive failed: {}", defmt::Debug2Format(&e));
            core::panic!("mesh rx: receive failed");
        }
    };
    let mut buf = [0u8; 128];
    let mut count = 0u32;
    for _ in 0..20 {
        match receiving.r_wait(&mut buf).await {
            Ok(msg) => {
                if let Some(incoming) = decode_mesh_frame(&msg) {
                    hw.lock().push_incoming(incoming);
                    count += 1;
                }
                break;
            }
            Err(nb::Error::WouldBlock) => Timer::after_micros(100).await,
            Err(nb::Error::Other(e)) => {
                if !is_retryable_rx_error(&e) {
                    error!(
                        "mesh rx: non-retryable radio error: {}",
                        defmt::Debug2Format(&e)
                    );
                    core::panic!("mesh rx: unexpected radio error");
                }
                break;
            }
        }
    }
    let dw = finish_receiving_or_panic(receiving).await;
    (dw, count)
}

/// Decode one received 802.15.4 frame into a mesh `IncomingMessage`.
/// Returns `None` for non-data frames or malformed packets.
fn decode_mesh_frame(msg: &dw3000_ng::hl::Message<'_>) -> Option<IncomingMessage> {
    let mut bytes = msg.frame.clone().into_inner().to_vec();
    // The DW3000 reports rxflen including the 2-byte FCS, so the trailing
    // two bytes are the frame check sequence, not application data. Strip
    // them before the mesh parses the payload.
    if bytes.len() < 2 {
        return None;
    }
    bytes.truncate(bytes.len() - 2);
    let frame = Ieee802154Frame::new_checked(&bytes[..]).ok()?;
    let repr = Ieee802154Repr::parse(&frame).ok()?;
    let src = match repr.src_addr? {
        Ieee802154Address::Short(s) => RealHardwareInterface::map_source(s),
        Ieee802154Address::Extended(e) => UwbAddress::Extended(e),
        Ieee802154Address::Absent => return None,
    };
    let payload = frame.payload().unwrap_or(&[]);
    Some(IncomingMessage {
        source: src,
        data: payload.to_vec(),
    })
}

/// Transmit every frame queued in the hardware interface. Returns the radio
/// back plus the number of frames sent.
async fn drain_tx(
    mut dw: DwReady,
    hw: Arc<Mutex<dyn HardwareInterface>>,
    src_short: [u8; 2],
    seq: &mut u8,
) -> (DwReady, u32) {
    let mut count = 0u32;
    loop {
        let next = hw.lock().take_outgoing();
        let Some((destination, data)) = next else {
            break;
        };
        let dst_short = RealHardwareInterface::map_destination(&destination);
        dw = mesh_tx_once(dw, src_short, dst_short, *seq, &data).await;
        *seq = seq.wrapping_add(1);
        count += 1;
    }
    (dw, count)
}

/// Transmit one mesh payload as an 802.15.4 data frame.
async fn mesh_tx_once(
    dw: DwReady,
    src_short: [u8; 2],
    dst_short: [u8; 2],
    seq: u8,
    payload: &[u8],
) -> DwReady {
    let cfg = dw_rf_config();
    let repr = Ieee802154Repr {
        frame_type: Ieee802154FrameType::Data,
        security_enabled: false,
        frame_pending: false,
        ack_request: false,
        sequence_number: Some(seq),
        pan_id_compression: true,
        frame_version: Ieee802154FrameVersion::Ieee802154_2006,
        dst_pan_id: Some(Ieee802154Pan(PAN_ID)),
        dst_addr: Some(Ieee802154Address::Short(dst_short)),
        src_pan_id: None,
        src_addr: Some(Ieee802154Address::Short(src_short)),
    };
    let header_len = repr.buffer_len();
    let total = header_len + payload.len();
    let mut buf = [0u8; 128];
    let frame = {
        let mut f = Ieee802154Frame::new_unchecked(&mut buf[..total]);
        repr.emit(&mut f);
        f.payload_mut()
            .expect("mesh tx: no payload area")
            .copy_from_slice(payload);
        f
    };
    let mut sending = match dw.send_frame(frame, SendTime::Now, cfg).await {
        Ok(s) => s,
        Err(e) => {
            error!("mesh tx: send_frame failed: {}", defmt::Debug2Format(&e));
            core::panic!("mesh tx: send_frame failed");
        }
    };
    if let Err(e) = wait_tx(&mut sending).await {
        error!("mesh tx: s_wait failed: {}", defmt::Debug2Format(&e));
        core::panic!("mesh tx: s_wait failed");
    }
    match sending.finish_sending().await {
        Ok(d) => d,
        Err((s, e)) => {
            error!(
                "mesh tx: finish_sending failed: {}",
                defmt::Debug2Format(&e)
            );
            match s.finish_sending().await {
                Ok(d) => d,
                Err(_) => core::panic!("mesh tx: finish_sending failed twice"),
            }
        }
    }
}

/// Clock tree for the STM32L431 board: HSI-derived 72 MHz sysclk via PLL1_R.
#[cfg(feature = "board-l431")]
fn board_rcc_config(config: &mut Config) {
    use embassy_stm32::rcc::{Pll, PllMul, PllPreDiv, PllQDiv, PllRDiv, PllSource, Sysclk};

    config.rcc.sys = Sysclk::PLL1_R;
    config.rcc.hsi = true;
    // Enable LSI so the IWDG watchdog can run.
    config.rcc.ls.lsi = true;
    config.rcc.pll = Some(Pll {
        source: PllSource::HSI,
        prediv: PllPreDiv::DIV1,
        mul: PllMul::MUL18,
        divp: None,
        divq: Some(PllQDiv::DIV6), // 48Mhz (16 / 1 * 18 / 6)
        divr: Some(PllRDiv::DIV4), // sysclk 72Mhz clock (16 / 1 * 18 / 4)
    });
}

/// Clock tree for the Ai-Thinker BU03-Kit (STM32F103CBT6): 8 MHz HSE crystal
/// (X1) driving a 72 MHz sysclk via PLL x9, standard "blue pill"-style config.
#[cfg(feature = "board-bu03")]
fn board_rcc_config(config: &mut Config) {
    use embassy_stm32::rcc::{
        AHBPrescaler, APBPrescaler, Hse, HseMode, Pll, PllMul, PllPreDiv, PllSource, Sysclk,
    };

    config.rcc.hse = Some(Hse {
        freq: Hertz(8_000_000),
        mode: HseMode::Oscillator,
    });
    config.rcc.pll = Some(Pll {
        src: PllSource::HSE,
        prediv: PllPreDiv::DIV1,
        mul: PllMul::MUL9, // sysclk 72Mhz clock (8 / 1 * 9)
    });
    config.rcc.sys = Sysclk::PLL1_P;
    config.rcc.ahb_pre = AHBPrescaler::DIV1;
    config.rcc.apb1_pre = APBPrescaler::DIV2;
    config.rcc.apb2_pre = APBPrescaler::DIV1;
}

/// DW3000 RSTN is wired to PB2 on the STM32L431 board.
#[cfg(feature = "board-l431")]
fn reset_pin(p: &embassy_stm32::Peripherals) -> Peri<'static, impl Pin> {
    unsafe { p.PB2.clone_unchecked() }
}

/// DW3000 RSTN is wired to PA0 (net "DW_RSTN") on the Ai-Thinker BU03-Kit,
/// per the BU03-Kit schematic (STM32F103CBT6 U1, pin 10 / PA0-WKUP).
#[cfg(feature = "board-bu03")]
fn reset_pin(p: &embassy_stm32::Peripherals) -> Peri<'static, impl Pin> {
    unsafe { p.PA0.clone_unchecked() }
}

/// Build (and initialize) a DW3000 by cloning the shared peripheral handles,
/// configuring it with the given short address. Safe to call repeatedly for
/// a full hardware reset + re-init cycle.
async fn build_dw(p: &embassy_stm32::Peripherals, addr: [u8; 2]) -> DwReady {
    unsafe {
        init_dw3000(
            p.SPI1.clone_unchecked(),
            p.PA5.clone_unchecked(),
            p.PA7.clone_unchecked(),
            p.PA6.clone_unchecked(),
            p.DMA1_CH3.clone_unchecked(),
            p.DMA1_CH2.clone_unchecked(),
            p.PA4.clone_unchecked(),
            reset_pin(p),
            addr,
        )
        .await
    }
}

type DwBus<'d> = ExclusiveDevice<Spi<'d, Async, spi::mode::Master>, Output<'d>, NoDelay>;
type DwError = dw3000_ng::Error<DwBus<'static>>;
type DwReady = DW3000<DwBus<'static>, Ready>;
type DwSending = DW3000<DwBus<'static>, Sending>;
type DwReceiving = DW3000<DwBus<'static>, SingleBufferReceiving>;

async fn init_dw3000<'d, D1, D2>(
    spi1: Peri<'d, SPI1>,
    sck: Peri<'d, peripherals::PA5>,
    mosi: Peri<'d, peripherals::PA7>,
    miso: Peri<'d, peripherals::PA6>,
    tx_dma: Peri<'d, D1>,
    rx_dma: Peri<'d, D2>,
    cs: Peri<'d, impl Pin>,
    reset: Peri<'d, impl Pin>,
    addr: [u8; 2],
) -> DW3000<DwBus<'d>, Ready>
where
    D1: spi::TxDma<SPI1>,
    D2: spi::RxDma<SPI1>,
    Irqs: Binding<<D1 as embassy_stm32::dma::ChannelInstance>::Interrupt, dma::InterruptHandler<D1>>
        + Binding<<D2 as embassy_stm32::dma::ChannelInstance>::Interrupt, dma::InterruptHandler<D2>>,
{
    info!("init_dw3000: resetting DW3000 via PB2");
    {
        let rst = Output::new(reset, Level::Low, Speed::Low);
        info!("init_dw3000: reset pin driven low, busy-waiting ~5ms");
        cortex_m::asm::delay(360_000); // ~5ms @ 72MHz, diagnostic only
        info!("init_dw3000: busy-wait done, releasing reset");
        drop(rst);
    }
    info!("init_dw3000: waiting 1ms after release");
    Timer::after(Duration::from_millis(1)).await;
    info!("init_dw3000: 1ms elapsed");

    info!("init_dw3000: configuring SPI1 (PA5=SCK, PA6=MISO, PA7=MOSI, mode 0, 18 MHz)");
    let mut cfg = spi::Config::default();
    cfg.mode = spi::MODE_0;
    cfg.frequency = Hertz(18_000_000);
    let spi = Spi::new(spi1, sck, mosi, miso, tx_dma, rx_dma, Irqs, cfg);
    info!("init_dw3000: SPI1 ready, chip select on CS pin");

    let cs = Output::new(cs, Level::High, Speed::VeryHigh);
    let dev = ExclusiveDevice::new_no_delay(spi, cs);

    let dw = DW3000::new(dev);
    info!("init_dw3000: calling dw3000.init()");
    let dw = match dw.init().await {
        Ok(dw) => dw,
        Err(e) => {
            error!("dw3000 init failed: {}", defmt::Debug2Format(&e));
            core::panic!("dw3000 init failed");
        }
    };
    info!("init_dw3000: calling dw3000.config()");
    let mut dw = match dw.config(DwConfig::default(), AsyncDelay).await {
        Ok(dw) => dw,
        Err(e) => {
            error!("dw3000 config failed: {}", defmt::Debug2Format(&e));
            core::panic!("dw3000 config failed");
        }
    };

    let mut dw = match dw
        .set_address(Ieee802154Pan(PAN_ID), Ieee802154Address::Short(addr))
        .await
    {
        Ok(()) => dw,
        Err(e) => {
            error!("set_address failed: {}", defmt::Debug2Format(&e));
            core::panic!("set_address failed");
        }
    };
    let dw = match dw.set_antenna_delay(0, 0).await {
        Ok(()) => dw,
        Err(e) => {
            error!("set_antenna_delay failed: {}", defmt::Debug2Format(&e));
            core::panic!("set_antenna_delay failed");
        }
    };

    info!("init_dw3000: DW3000 initialized successfully");
    dw
}

/// Enter DW3000 true DEEPSLEEP (all clocks off, ~260 nA).
/// Wake-up is a full hardware reset + re-init via `build_dw`.
async fn dw_deep_sleep(dw: &mut DwReady) {
    if let Err(e) = dw
        .ll()
        .aon_cfg()
        .write(|w| {
            w.sleep_en(1)
                .wake_cnt(0)
                .wake_csn(0)
                .wake_wup(0)
                .pres_sleep(0)
        })
        .await
    {
        warn!(
            "dw_deep_sleep: aon_cfg write failed: {}",
            defmt::Debug2Format(&e)
        );
    }
    if let Err(e) = dw.ll().aon_dig_cfg().write(|w| w.onw_aon_dld(1)).await {
        warn!(
            "dw_deep_sleep: aon_dig_cfg write failed: {}",
            defmt::Debug2Format(&e)
        );
    }
    if let Err(e) = dw.ll().aon_ctrl().write(|w| w).await {
        warn!(
            "dw_deep_sleep: aon_ctrl write failed: {}",
            defmt::Debug2Format(&e)
        );
    }
    if let Err(e) = dw.ll().aon_ctrl().write(|w| w.save(1)).await {
        warn!(
            "dw_deep_sleep: aon_ctrl save write failed: {}",
            defmt::Debug2Format(&e)
        );
    }
    info!("dw: DEEPSLEEP issued");
}

struct AsyncDelay;

impl embedded_hal_async::delay::DelayNs for AsyncDelay {
    async fn delay_ns(&mut self, ns: u32) {
        Timer::after(Duration::from_nanos(ns as u64)).await;
    }
}

// ---------------------------------------------------------------------------
// Frame helpers
// ---------------------------------------------------------------------------

/// RF config for all TX/RX: default channel 5, PRF 64 MHz, preamble 128,
/// 850 kbps, with the ranging bit set.
fn dw_rf_config() -> DwConfig {
    DwConfig {
        ranging_enable: true,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// TX/RX waits
//
// `wait_tx` spins until the DW3000 reports the frame as sent; it is shared
// by the mesh radio pump (`mesh_tx_once`).
// ---------------------------------------------------------------------------

async fn wait_tx(dw: &mut DwSending) -> Result<DwInstant, DwError> {
    loop {
        match dw.s_wait().await {
            Ok(ts) => return Ok(ts),
            Err(nb::Error::WouldBlock) => Timer::after_micros(20).await,
            Err(nb::Error::Other(e)) => return Err(e),
        }
    }
}

/// RF-level failures we can simply retry; anything else is a fatal radio error.
fn is_retryable_rx_error(e: &DwError) -> bool {
    matches!(
        e,
        DwError::FrameWaitTimeout
            | DwError::PreambleDetectionTimeout
            | DwError::SfdTimeout
            | DwError::Phy
            | DwError::ReedSolomon
            | DwError::Overrun
            | DwError::FrameFilteringRejection
            | DwError::Fcs
            | DwError::Frame(_)
    )
}
