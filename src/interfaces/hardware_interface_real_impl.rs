extern crate alloc;

use alloc::vec::Vec;

use crate::{
    interfaces::hardware_interface::{
        HardwareError, HardwareInterface, IncomingMessage, UwbAddress,
    },
    prng,
};

/// Max frames buffered from the radio before the mesh drains them.
const RX_QUEUE_CAP: usize = 8;
/// Max frames waiting for the async TX pump. Oldest is dropped on overflow so
/// a sleeping responder's queued OGMs can't OOM the heap after wake-up.
const TX_QUEUE_CAP: usize = 16;
/// Largest mesh payload accepted. An 802.15.4 PSDU is 127 bytes including the
/// ~11 byte header and 2 byte FCS, so 100 leaves ample margin.
const MAX_PAYLOAD_LEN: usize = 100;

/// Real-hardware `HardwareInterface` for the DW3000 boards.
///
/// Design: this struct owns **no radio hardware**. It is a synchronous
/// queue bridge between the (sync) mesh and the (async) radio pump in
/// `main.rs`, which owns the `DW3000` driver:
///
/// - `send_message` enqueues into the TX queue and returns immediately, so
///   `Device::heartbeat()` never blocks.
/// - the async pump in `main` drains via `take_outgoing`, transmits each
///   frame over UWB, polls the receiver, and pushes decoded frames back via
///   `push_incoming`.
/// - `get_incoming_message` pops the RX queue for the mesh.
///
/// Address mapping: the radio uses 802.15.4 short addresses. Our short is
/// given at construction; `get_uwb_address` exposes it canonically as
/// `Extended([s0, s1, 0, 0, 0, 0, 0, 0])` because the mesh serialises every
/// address as 8 bytes (`Message::to_data`) and parses them back as
/// `Extended`, so returning `Short` here would never compare equal on the
/// receiver. `Extended([0xFF; 8])` maps to the 802.15.4 broadcast address.
pub struct RealHardwareInterface {
    own_ext: UwbAddress,
    prng: prng::Prng,
    rx_queue: Vec<IncomingMessage>,
    tx_queue: Vec<(UwbAddress, Vec<u8>)>,
}

impl RealHardwareInterface {
    pub fn new(short_address: [u8; 2], seed: u32) -> Self {
        Self {
            own_ext: UwbAddress::Extended([
                short_address[0],
                short_address[1],
                0,
                0,
                0,
                0,
                0,
                0,
            ]),
            prng: prng::Prng::new(seed),
            rx_queue: Vec::new(),
            tx_queue: Vec::new(),
        }
    }

    /// Map a mesh destination to the 802.15.4 short to transmit to.
    pub fn map_destination(destination: &UwbAddress) -> [u8; 2] {
        match destination {
            UwbAddress::Short(v) => v.to_be_bytes(),
            UwbAddress::Extended(b) => {
                if *b == [0xFF; 8] {
                    [0xFF, 0xFF]
                } else {
                    [b[0], b[1]]
                }
            }
        }
    }

    /// Map a received 802.15.4 short source to the canonical mesh address.
    pub fn map_source(short: [u8; 2]) -> UwbAddress {
        UwbAddress::Extended([short[0], short[1], 0, 0, 0, 0, 0, 0])
    }
}

impl HardwareInterface for RealHardwareInterface {
    fn printf(&self, args: core::fmt::Arguments<'_>) {
        let s = alloc::format!("{}", args);
        defmt::info!("{=str}", s.as_str());
    }

    fn is_serial_input_available(&self) -> bool {
        false
    }

    fn read_char_over_serial(&self) -> Option<char> {
        None
    }

    fn millis(&self) -> u32 {
        embassy_time::Instant::now().as_millis() as u32
    }

    fn is_alive(&self) -> bool {
        true
    }

    fn lock_mutex(&mut self, _mutex_id: u32, _timeout_ms: u32) -> bool {
        false
    }

    fn unlock_mutex(&mut self, _mutex_id: u32) -> bool {
        false
    }

    fn get_uwb_address(&self) -> UwbAddress {
        self.own_ext
    }

    fn init_messages(&mut self) -> HardwareError {
        self.rx_queue.clear();
        self.tx_queue.clear();
        HardwareError::Ok
    }

    fn get_incoming_message(&mut self) -> Option<IncomingMessage> {
        if self.rx_queue.is_empty() {
            None
        } else {
            Some(self.rx_queue.remove(0))
        }
    }

    fn send_message(&mut self, destination: UwbAddress, data: &[u8]) -> HardwareError {
        if data.len() > MAX_PAYLOAD_LEN {
            return HardwareError::SoftwareBug;
        }
        if self.tx_queue.len() >= TX_QUEUE_CAP {
            self.tx_queue.remove(0);
        }
        self.tx_queue.push((destination, data.to_vec()));
        HardwareError::Ok
    }

    fn get_number_of_queued_messages(&self) -> i32 {
        self.rx_queue.len() as i32
    }

    fn generate_random_integer(&mut self) -> u32 {
        self.prng.next_u32()
    }

    fn push_incoming(&mut self, msg: IncomingMessage) {
        if self.rx_queue.len() >= RX_QUEUE_CAP {
            self.rx_queue.remove(0);
        }
        self.rx_queue.push(msg);
    }

    fn take_outgoing(&mut self) -> Option<(UwbAddress, Vec<u8>)> {
        if self.tx_queue.is_empty() {
            None
        } else {
            Some(self.tx_queue.remove(0))
        }
    }
}
