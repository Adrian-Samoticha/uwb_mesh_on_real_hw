extern crate alloc;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;
use spin::Mutex;

use crate::device::array_map::ArrayMap;
use crate::interfaces::hardware_interface::{self, UwbAddress, UwbAddressImpl};

type SharedVec<T> = Rc<RefCell<Vec<T>>>;

// Initially, OGM messages were sent over unicast, rather than broadcast,
// in order to improve reliability. To this end, the network used probe
// requests, probe responses, and partner requests to establish reliable unicast
// paths before sending OGMs. The mesh would work in two phases:
// 1. Scan the network for other devices using probe requests and responses.
// 2. Maintain partner relationships and reliable unicast paths with other
// devices.
// During testing in the simulator, I have found that this approach could fail
// if no other devices were found in the first phase. The mesh would then
// effectively split in two. For this reason, this approach was abandoned in
// favor of broadcast OGMs. The code that implemented the unicast-based approach
// was merely commented out rather than deleted, in case it needed to be
// referenced or reinstated later.

/// Represents a message that can be sent or received in the mesh network.
enum Message {
    // /// A probe request message, used to discover other devices in the mesh network.
    // ProbeRequest {},

    // /// A probe response message, sent in response to a probe request.
    // ProbeResponse {},

    // /// A partner request message, used to establish a partnership between devices.
    // /// A partner is a device that has been dedicated as a network edge and is maintained as such.
    // PartnerRequest {},
    /// An OGM (Originator Message) message, used for routing in the mesh network.
    Ogm {
        source: UwbAddress,
        sequence_number: u32,
    },

    /// A unicast message, sent from one device to another in the mesh network.
    Unicast {
        source: UwbAddress,
        previous_hop: UwbAddress,
        destination: UwbAddress,
        sequence_number: u32,
        data: Vec<u8>,
    },

    /// An acknowledgment message for a unicast message.
    Ack {
        source: UwbAddress,
        destination: UwbAddress,
        sequence_number: u32,
    },
}

impl Message {
    fn to_data(&self) -> Vec<u8> {
        match self {
            // Message::ProbeRequest {} => vec![0x01],
            // Message::ProbeResponse {} => vec![0x02],
            // Message::PartnerRequest {} => vec![0x03],
            Message::Ogm {
                source,
                sequence_number,
            } => {
                let mut data = vec![0x04];
                data.extend_from_slice(&source.bytes());
                data.extend_from_slice(&sequence_number.to_be_bytes());
                data
            }
            Message::Unicast {
                source,
                previous_hop,
                destination,
                sequence_number,
                data,
            } => {
                let mut result = vec![0x05];
                result.extend_from_slice(&source.bytes());
                result.extend_from_slice(&previous_hop.bytes());
                result.extend_from_slice(&destination.bytes());
                result.extend_from_slice(&sequence_number.to_be_bytes());
                result.extend_from_slice(data);
                result
            }
            Message::Ack {
                source,
                destination,
                sequence_number,
            } => {
                let mut result = vec![0x06];
                result.extend_from_slice(&source.bytes());
                result.extend_from_slice(&destination.bytes());
                result.extend_from_slice(&sequence_number.to_be_bytes());
                result
            }
        }
    }

    fn from_data(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        match data.first().copied() {
            // Some(0x01) => Some(Message::ProbeRequest {}),
            // Some(0x02) => Some(Message::ProbeResponse {}),
            // Some(0x03) => Some(Message::PartnerRequest {}),
            Some(0x04) => {
                if data.len() < 1 + 8 + 4 {
                    return None;
                }
                let source_bytes = &data[1..9];
                let sequence_number_bytes = &data[9..13];

                let source = UwbAddress::Extended(source_bytes.try_into().ok()?);
                let sequence_number = u32::from_be_bytes(sequence_number_bytes.try_into().ok()?);

                Some(Message::Ogm {
                    source,
                    sequence_number,
                })
            }
            Some(0x05) => {
                if data.len() < 1 + 8 + 8 + 8 + 4 {
                    return None;
                }
                let source_bytes = &data[1..9];
                let previous_hop_bytes = &data[9..17];
                let destination_bytes = &data[17..25];
                let sequence_number_bytes = &data[25..29];
                let source = UwbAddress::Extended(source_bytes.try_into().ok()?);
                let previous_hop = UwbAddress::Extended(previous_hop_bytes.try_into().ok()?);
                let destination = UwbAddress::Extended(destination_bytes.try_into().ok()?);
                let sequence_number = u32::from_be_bytes(sequence_number_bytes.try_into().ok()?);
                let payload = data[29..].to_vec();

                Some(Message::Unicast {
                    source,
                    previous_hop,
                    destination,
                    sequence_number,
                    data: payload,
                })
            }
            Some(0x06) => {
                if data.len() < 1 + 8 + 8 + 4 {
                    return None;
                }
                let source_bytes = &data[1..9];
                let destination_bytes = &data[9..17];
                let sequence_number_bytes = &data[17..21];
                let source = UwbAddress::Extended(source_bytes.try_into().ok()?);
                let destination = UwbAddress::Extended(destination_bytes.try_into().ok()?);
                let sequence_number = u32::from_be_bytes(sequence_number_bytes.try_into().ok()?);

                Some(Message::Ack {
                    source,
                    destination,
                    sequence_number,
                })
            }
            _ => None,
        }
    }
}

// /// Represents the current stage of the mesh network.
// enum MeshStage {
//     /// The mesh network is in the process of initializing connections with other devices.
//     InitConnections {
//         time_until_done_in_ms: i32,
//         time_until_broadcast_in_ms: i32,
//         found_devices: Vec<hardware_interface::UwbAddress>,
//     },

//     /// The mesh network is running and maintaining connections with other devices.
//     Running,
// }

/// An outgoing message that is scheduled to be sent after a certain delay.
#[derive(Clone)]
struct OutgoingMessage {
    destination: hardware_interface::UwbAddress,
    data: SharedVec<u8>,
    time_to_send_in_ms: i32,
}

/// Handles outgoing messages, scheduling them to be sent after a certain delay.
struct OutgoingMessageHandler {
    messages: Vec<OutgoingMessage>,
}

impl OutgoingMessageHandler {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    fn add_message(&mut self, message: OutgoingMessage) {
        self.messages.push(message);
    }

    /// Processes the messages and returns a vector of messages that are ready
    /// to be sent.
    fn process_messages(&mut self, time_passed_in_ms: i32) -> Vec<OutgoingMessage> {
        let mut ready_messages = Vec::new();

        for message in &mut self.messages {
            message.time_to_send_in_ms -= time_passed_in_ms;

            if message.time_to_send_in_ms <= 0 {
                ready_messages.push(OutgoingMessage {
                    destination: message.destination,
                    data: message.data.clone(),
                    time_to_send_in_ms: 0,
                });
            }
        }
        self.messages
            .retain(|message| message.time_to_send_in_ms > 0);

        ready_messages
    }
}

#[derive(Clone)]
struct OutgoingMessageWithRetryCountAndSequenceNumber {
    message: OutgoingMessage,
    retries_left: u16,
    sequence_number: u32,
}

struct OutgoingMessageRetryHandler {
    messages_with_retry_count: Vec<OutgoingMessageWithRetryCountAndSequenceNumber>,
    unicast_max_retry_number: u16,
}

impl OutgoingMessageRetryHandler {
    fn new(unicast_max_retry_number: u16) -> Self {
        Self {
            messages_with_retry_count: Vec::new(),
            unicast_max_retry_number,
        }
    }

    /// Sets the maximum number of retry attempts for unicast messages.
    fn set_unicast_max_retry_number(&mut self, unicast_max_retry_number: u16) {
        self.unicast_max_retry_number = unicast_max_retry_number;
    }

    /// Gets the maximum number of retry attempts for unicast messages.
    fn get_unicast_max_retry_number(&self) -> u16 {
        self.unicast_max_retry_number
    }

    fn add_message(&mut self, message: OutgoingMessage, sequence_number: u32) {
        let element = OutgoingMessageWithRetryCountAndSequenceNumber {
            message,
            retries_left: self.unicast_max_retry_number,
            sequence_number,
        };
        self.messages_with_retry_count.push(element);
    }

    /// Processes the messages with retry counts and returns a vector of messages that are ready to be sent.
    /// # Arguments
    ///
    /// * `time_passed_in_ms` - The amount of time that has passed since the last processing step, in milliseconds.
    /// * `random_ms_number` - The random number of milliseconds to set for the next retry of a message.
    ///
    /// # Returns
    ///
    /// A vector of messages that are ready to be sent.
    fn process_messages(
        &mut self,
        time_passed_in_ms: i32,
        random_ms_number: i32,
    ) -> Vec<OutgoingMessage> {
        let mut ready_messages = Vec::new();

        for message in &mut self.messages_with_retry_count {
            message.message.time_to_send_in_ms -= time_passed_in_ms;

            if message.message.time_to_send_in_ms <= 0 {
                ready_messages.push(OutgoingMessage {
                    destination: message.message.destination,
                    data: message.message.data.clone(),
                    time_to_send_in_ms: 0,
                });
                message.message.time_to_send_in_ms = random_ms_number;
                message.retries_left -= 1;
            }
        }

        self.messages_with_retry_count
            .retain(|message| message.retries_left > 0);

        ready_messages
    }

    /// Acknowledges the receipt of a message with the given sequence number and destination, removing it from the retry handler.
    fn ack(&mut self, sequence_number: u32, destination: hardware_interface::UwbAddress) {
        self.messages_with_retry_count.retain(|message| {
            message.sequence_number != sequence_number || message.message.destination != destination
        });
    }
}

#[derive(Eq, Hash, PartialEq, Clone, Copy)]
struct UwbAddressAndSequenceNumber {
    address: hardware_interface::UwbAddress,
    sequence_number: u32,
}

#[derive(Eq, Hash, PartialEq, Clone, Copy)]
struct SourceDestinationSequenceNumberAndEntryAge {
    source: hardware_interface::UwbAddress,
    destination: hardware_interface::UwbAddress,
    sequence_number: u32,
    entry_age_in_ms: u32,
}

pub struct Mesh {
    /// The hardware interface used to interface with the underlying hardware.
    hardware_interface: Option<Arc<Mutex<dyn hardware_interface::HardwareInterface>>>,

    /// The time of the last step in milliseconds.
    time_of_last_step: u32,

    /// The time in steps, where each step is 10 milliseconds.
    time_in_steps: u32,

    // /// The current stage of the mesh network.
    // stage: MeshStage,
    /// The outgoing message handler, which manages messages that are scheduled to be sent.
    outgoing_message_handler: OutgoingMessageHandler,

    /// The outgoing message retry handler, which manages messages that need to be retried.
    outgoing_message_retry_handler: OutgoingMessageRetryHandler,

    // /// List of devices that have been found during the InitConnections stage
    // /// and have been dedicated as partners.
    // /// A partner is a device for which a network edge has been established and
    // /// is maintained.
    // partners: Vec<hardware_interface::UwbAddress>,

    // /// List of devices for which this device is a partner.
    // partners_of: Vec<hardware_interface::UwbAddress>,
    /// The next sequence number to use for OGM messages.
    next_ogm_sequence_number: u32,

    /// The next sequence number to use for unicast messages.
    next_unicast_sequence_number: u32,

    /// Maps received OGM messages to their age in milliseconds. This is used to determine when to discard old OGMs.
    received_ogm_to_age_in_ms: [(UwbAddressAndSequenceNumber, u32); 4],

    /// Addressed-to-me unicast payloads, delivered from the radio during
    /// `heartbeat()` and drained by the application with
    /// `take_received_data()`. The mesh only transports these payloads; the
    /// application owns any acknowledgement/retransmission protocol.
    received_data: Vec<(hardware_interface::UwbAddress, Vec<u8>)>,

    /// The routing table, which maps destination addresses to the next hop address.
    routing_table: ArrayMap<hardware_interface::UwbAddress, hardware_interface::UwbAddress, 4>,

    /// How often to repeat OGM transmissions (number of times each OGM is sent).
    ogm_burst_number: u16,

    /// Tracks received unicast messages to prevent double delivery.
    received_unicast_messages: Vec<SourceDestinationSequenceNumberAndEntryAge>,
}

impl Mesh {
    pub fn new() -> Self {
        Self {
            hardware_interface: None,
            time_of_last_step: 0,
            time_in_steps: 0,
            // stage: MeshStage::InitConnections {
            //     time_until_done_in_ms: 1000,
            //     time_until_broadcast_in_ms: 0,
            //     found_devices: Vec::new(),
            // },
            outgoing_message_handler: OutgoingMessageHandler::new(),
            outgoing_message_retry_handler: OutgoingMessageRetryHandler::new(64),
            // partners: Vec::new(),
            // partners_of: Vec::new(),
            next_ogm_sequence_number: 0,
            next_unicast_sequence_number: 0,
            received_ogm_to_age_in_ms: [(
                UwbAddressAndSequenceNumber {
                    address: hardware_interface::UwbAddress::Short(0),
                    sequence_number: 0,
                },
                0,
            ); 4],
            received_data: Vec::new(),
            routing_table: ArrayMap::new([None; 4], [hardware_interface::UwbAddress::Short(0); 4]),
            ogm_burst_number: 8,
            received_unicast_messages: Vec::new(),
        }
    }

    // /// Randomizes the time until the next broadcast in the InitConnections stage.
    // fn randomize_time_until_broadcast(&mut self) {
    //     let hi = self.hardware_interface.as_ref().unwrap().lock();
    //     let random_value = (hi.generate_random_integer() % 50) * 10 + 10;

    //     if let MeshStage::InitConnections {
    //         time_until_done_in_ms: _,
    //         time_until_broadcast_in_ms,
    //         found_devices: _,
    //     } = &mut self.stage
    //     {
    //         *time_until_broadcast_in_ms = random_value as i32;
    //     }
    // }

    /// Initializes the mesh network with the given hardware interface.
    pub fn init(
        &mut self,
        hardware_interface: Arc<Mutex<dyn hardware_interface::HardwareInterface>>,
    ) {
        self.hardware_interface = Some(hardware_interface);

        let hi = self.hardware_interface.as_ref().unwrap();

        hi.lock().init_messages();

        // self.randomize_time_until_broadcast();
    }

    /// Heartbeat method. To be called as often as possible.
    pub fn heartbeat(&mut self) {
        assert!(self.hardware_interface.is_some(), "Mesh not initialized");

        self.receive_messages();

        loop {
            let hi = self.hardware_interface.as_ref().unwrap().lock();
            let elapsed = hi.millis() - self.time_of_last_step;
            drop(hi);

            if elapsed >= 10 {
                self.time_of_last_step += 10;
                self.time_in_steps += 1;
                self.on_10ms_passed();
            } else {
                break;
            }
        }
    }

    /// Called every 10 milliseconds to handle time-based events in the mesh network.
    fn on_10ms_passed(&mut self) {
        // let mut should_broadcast_probe = false;

        let delay_until_next_retry = {
            let mut hi = self.hardware_interface.as_ref().unwrap().lock();
            let delay = (hi.generate_random_integer() % 4 + 2) * 10;
            drop(hi);
            delay
        };

        self.outgoing_message_retry_handler
            .process_messages(10, delay_until_next_retry.try_into().unwrap())
            .into_iter()
            .for_each(|message| {
                self.outgoing_message_handler.add_message(message);
            });

        self.outgoing_message_handler
            .process_messages(10)
            .into_iter()
            .for_each(|message| {
                let mut hi = self.hardware_interface.as_mut().unwrap().lock();
                let arr = &message.data.borrow();
                hi.send_message(message.destination, arr);
                drop(hi);
            });

        self.received_unicast_messages.iter_mut().for_each(|entry| {
            entry.entry_age_in_ms += 10;
        });
        self.received_unicast_messages
            .retain(|entry| entry.entry_age_in_ms < 5000);
        while self.received_unicast_messages.len() > 20 {
            let oldest_entry_index = self
                .received_unicast_messages
                .iter()
                .enumerate()
                .max_by_key(|(_, entry)| entry.entry_age_in_ms)
                .map(|(index, _)| index);
            if let Some(index) = oldest_entry_index {
                self.received_unicast_messages.remove(index);
            }
        }

        // if let MeshStage::InitConnections {
        //     time_until_done_in_ms,
        //     time_until_broadcast_in_ms,
        //     found_devices,
        // } = &mut self.stage
        // {
        //     if *time_until_done_in_ms > 0 {
        //         *time_until_done_in_ms -= 10;
        //     }

        //     if *time_until_broadcast_in_ms > 0 {
        //         *time_until_broadcast_in_ms -= 10;

        //         if *time_until_broadcast_in_ms <= 0 {
        //             should_broadcast_probe = true;
        //         }
        //     }

        //     if *time_until_done_in_ms <= 0 {
        //         let hi = self.hardware_interface.as_ref().unwrap().lock();
        //         let random_number = hi.generate_random_integer() % 100;
        //         let number_of_partners = if random_number < 10 { 2 } else { 1 };
        //         for i in 0..number_of_partners {
        //             if let Some(partner) = found_devices.get(i) {
        //                 self.partners.push(*partner);
        //             }
        //         }

        //         hi.printf(format_args!(
        //             "InitConnections stage done. Found devices: {:?}; Partners: {:?}\n",
        //             found_devices, self.partners
        //         ));
        //         drop(hi);

        //         self.stage = MeshStage::Running;
        //     }
        // }

        // if should_broadcast_probe {
        //     let hi = self.hardware_interface.as_ref().unwrap().lock();
        //     hi.printf(format_args!("Broadcasting probe message\n"));
        //     drop(hi);

        //     self.broadcast_probe_request();
        // }

        // if let MeshStage::Running = &self.stage {
        if true {
            // if self.time_in_steps % 500 == 0 {
            //     let partners = self.partners.clone();
            //     for partner in partners {
            //         self.send_partner_request(&partner);
            //     }
            // }

            self.age_received_ogms(10);
        }
    }

    /// Handles incoming messages from the hardware interface, processing them according to their type.
    fn receive_messages(&mut self) {
        // Drain the queue first so the hardware interface lock is not held while
        // calling other &mut self methods (get_adjacent_devices, forward_ogm, etc.).
        let incoming_messages: Vec<_> = {
            let mut hi = self.hardware_interface.as_ref().unwrap().lock();
            let mut messages = Vec::new();
            while let Some(msg) = hi.get_incoming_message() {
                messages.push(msg);
            }
            messages
        };

        for incoming_message in incoming_messages {
            if let Some(message) = Message::from_data(&incoming_message.data) {
                match message {
                    // Message::ProbeRequest {} => {
                    //     let time_until_send_in_ms = {
                    //         let hi = self.hardware_interface.as_ref().unwrap().lock();
                    //         hi.printf(format_args!(
                    //             "Received probe message from {}\n",
                    //             incoming_message.source
                    //         ));
                    //         hi.generate_random_integer() % 50 * 10 + 10
                    //     };

                    //     let outgoing_message = OutgoingMessage {
                    //         destination: incoming_message.source,
                    //         data: Message::ProbeResponse {}.to_data(),
                    //         time_to_send_in_ms: time_until_send_in_ms as i32,
                    //     };
                    //     self.outgoing_message_handler.add_message(outgoing_message);
                    // }
                    // Message::ProbeResponse {} => {
                    //     {
                    //         let hi = self.hardware_interface.as_ref().unwrap().lock();
                    //         hi.printf(format_args!(
                    //             "Received probe response from {}\n",
                    //             incoming_message.source
                    //         ));
                    //     }

                    //     // if let MeshStage::InitConnections {
                    //     //     time_until_done_in_ms: _,
                    //     //     time_until_broadcast_in_ms: _,
                    //     //     found_devices,
                    //     // } = &mut self.stage
                    //     // {
                    //     //     if !found_devices.contains(&incoming_message.source) {
                    //     //         found_devices.push(incoming_message.source);
                    //     //     }
                    //     // }
                    // }
                    // Message::PartnerRequest {} => {
                    //     if !self.partners_of.contains(&incoming_message.source) {
                    //         self.partners_of.push(incoming_message.source);
                    //     }
                    // }
                    Message::Ogm {
                        source,
                        sequence_number,
                    } => {
                        let key = UwbAddressAndSequenceNumber {
                            address: source,
                            sequence_number,
                        };
                        if self
                            .received_ogm_to_age_in_ms
                            .iter()
                            .any(|(k, _)| k == &key)
                        {
                            let hi = self.hardware_interface.as_ref().unwrap().lock();
                            hi.printf(format_args!(
                                "Already received OGM from {} with sequence number {}, ignoring\n",
                                source, sequence_number
                            ));
                            continue;
                        }
                        if let Some(slot) =
                            self.received_ogm_to_age_in_ms.iter_mut().find(|(k, age)| {
                                (k.address == hardware_interface::UwbAddress::Short(0)
                                    && k.sequence_number == 0)
                                    || *age > 10000
                            })
                        {
                            *slot = (key, 0);
                        } else {
                            let hi = self.hardware_interface.as_ref().unwrap().lock();
                            hi.printf(format_args!(
                                "No available slot for OGM from {} with sequence number {}, ignoring\n",
                                source, sequence_number
                            ));
                            continue;
                        }

                        {
                            let hi = self.hardware_interface.as_ref().unwrap().lock();
                            hi.printf(format_args!(
                                "Received OGM from {} with sequence number {}\n",
                                source, sequence_number
                            ));
                        }

                        if !self.routing_table.contains_key(&source)
                            || self.routing_table[&source] != incoming_message.source
                        {
                            let hi = self.hardware_interface.as_ref().unwrap().lock();
                            hi.printf(format_args!(
                                "Updating routing table: {} -> {}\n",
                                source, incoming_message.source
                            ));

                            self.routing_table.insert(source, incoming_message.source);
                        }

                        // let adjacent_devices = self.get_adjacent_devices();
                        // for device in adjacent_devices {
                        //     if device == source {
                        //         continue;
                        //     }
                        //     self.forward_ogm(&device, source, sequence_number);
                        // }
                        let own_address = self
                            .hardware_interface
                            .as_ref()
                            .unwrap()
                            .lock()
                            .get_uwb_address();
                        if source != own_address {
                            let broadcast_address = hardware_interface::UwbAddress::new(
                                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                            );
                            self.forward_ogm(&broadcast_address, source, sequence_number);
                        }
                    }
                    Message::Unicast {
                        source,
                        previous_hop,
                        destination,
                        sequence_number,
                        data,
                    } => {
                        let ack_message = Message::Ack {
                            source: source,
                            destination: destination,
                            sequence_number,
                        }
                        .to_data();
                        let outgoing_message = OutgoingMessage {
                            destination: previous_hop,
                            data: Rc::new(RefCell::new(ack_message)),
                            time_to_send_in_ms: 0,
                        };
                        self.outgoing_message_handler.add_message(outgoing_message);

                        let already_received = self.received_unicast_messages.iter().any(|entry| {
                            entry.source == source
                                && entry.destination == destination
                                && entry.sequence_number == sequence_number
                        });

                        if already_received {
                            let hi = self.hardware_interface.as_ref().unwrap().lock();
                            hi.printf(format_args!(
                                "Already received unicast message from {} to {} with sequence number {}, ignoring\n",
                                source, destination, sequence_number
                            ));
                            continue;
                        }

                        self.received_unicast_messages.push(
                            SourceDestinationSequenceNumberAndEntryAge {
                                source,
                                destination,
                                sequence_number,
                                entry_age_in_ms: 0,
                            },
                        );

                        let is_addressed_to_me = {
                            let hi = self.hardware_interface.as_ref().unwrap().lock();
                            destination == hi.get_uwb_address()
                        };

                        if is_addressed_to_me {
                            {
                                let hi = self.hardware_interface.as_ref().unwrap().lock();
                                hi.printf(format_args!(
                                    "Received unicast message from {}: {:?}\n",
                                    source, data
                                ));
                            }
                            // Hand the payload to the application layer, which
                            // owns any acknowledgement logic.
                            self.received_data.push((source, data));
                        } else {
                            let next_hop = self.routing_table.get(&destination);
                            let hi = self.hardware_interface.as_ref().unwrap().lock();
                            let own_address = hi.get_uwb_address();
                            if let Some(next_hop) = next_hop {
                                let data = Message::Unicast {
                                    source,
                                    previous_hop: own_address,
                                    destination,
                                    sequence_number,
                                    data,
                                }
                                .to_data();
                                let outgoing_message = OutgoingMessage {
                                    destination: *next_hop,
                                    data: Rc::new(RefCell::new(data)),
                                    time_to_send_in_ms: 0,
                                };
                                self.outgoing_message_retry_handler
                                    .add_message(outgoing_message, sequence_number);

                                hi.printf(format_args!(
                                    "Forwarding unicast message from {} to {} via next hop {}\n",
                                    source, destination, next_hop
                                ));
                            } else {
                                hi.printf(format_args!(
                                    "No route to {} for unicast message from {}, dropping\n",
                                    destination, source
                                ));
                            }
                        }
                    }
                    Message::Ack {
                        source: _,
                        destination,
                        sequence_number,
                    } => {
                        self.outgoing_message_retry_handler
                            .ack(sequence_number, destination);

                        let hi = self.hardware_interface.as_ref().unwrap().lock();
                        hi.printf(format_args!(
                            "Received ack for sequence number {} from {}\n",
                            sequence_number, destination
                        ));
                    }
                }
            } else {
                let hi = self.hardware_interface.as_ref().unwrap().lock();
                hi.printf(format_args!(
                    "Received unknown message from {}: {:?}\n",
                    incoming_message.source, incoming_message.data
                ));
            }
        }
    }

    // /// Broadcasts a probe request message to discover other devices in the mesh network.
    // fn broadcast_probe_request(&mut self) {
    //     let message = Message::ProbeRequest {};
    //     let data = message.to_data();
    //     let broadcast_address =
    //         hardware_interface::UwbAddress::new(0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff);
    //     let time_until_send_in_ms = 0;

    //     let outgoing_message = OutgoingMessage {
    //         destination: broadcast_address,
    //         data,
    //         time_to_send_in_ms: time_until_send_in_ms,
    //     };
    //     self.outgoing_message_handler.add_message(outgoing_message);
    // }

    // /// Sends a partner request message to the specified partner device, establishing a partnership.
    // fn send_partner_request(&mut self, partner: &hardware_interface::UwbAddress) {
    //     let hi = self.hardware_interface.as_ref().unwrap().lock();

    //     let message = Message::PartnerRequest {};
    //     let data = message.to_data();
    //     let time_until_send_in_ms = hi.generate_random_integer() % 10 * 10;

    //     let outgoing_message = OutgoingMessage {
    //         destination: *partner,
    //         data,
    //         time_to_send_in_ms: time_until_send_in_ms as i32,
    //     };
    //     self.outgoing_message_handler.add_message(outgoing_message);
    // }

    // /// Returns a vector of adjacent devices, which includes both partners and devices for which this device is a partner.
    // fn get_adjacent_devices(&self) -> Vec<hardware_interface::UwbAddress> {
    //     let mut adjacent_devices = self.partners.clone();
    //     adjacent_devices.extend(self.partners_of.clone());
    //     adjacent_devices
    // }

    /// Floods the network with an OGM (Originator Message) to establish routing information.
    ///
    /// After the OGM message has flooded the network, the routing tables of all
    /// other devices will contain an entry for this device.
    pub fn send_ogm(&mut self) {
        // let adjacent_devices = self.get_adjacent_devices();
        // for device in adjacent_devices {
        let message = Message::Ogm {
            source: self
                .hardware_interface
                .as_ref()
                .unwrap()
                .lock()
                .get_uwb_address(),
            sequence_number: self.next_ogm_sequence_number,
        };
        let mut hi = self.hardware_interface.as_ref().unwrap().lock();
        let mut time_until_send_in_ms = 0;
        let message_data = Rc::new(RefCell::new(message.to_data()));
        for _ in 0..self.ogm_burst_number {
            let data = Rc::clone(&message_data);
            time_until_send_in_ms += hi.generate_random_integer() % 2 * 10;

            let broadcast_address =
                hardware_interface::UwbAddress::new(0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff);

            let outgoing_message = OutgoingMessage {
                // destination: device,
                destination: broadcast_address,
                data,
                time_to_send_in_ms: time_until_send_in_ms as i32,
            };
            self.outgoing_message_handler.add_message(outgoing_message);
        }
        // }

        self.next_ogm_sequence_number = self.next_ogm_sequence_number.wrapping_add(1);
    }

    // TODO: remove this (if it’s unused)
    // /// Queues one copy of a *single* OGM (one originator + sequence number)
    // /// as part of a flood. The main loop calls this once per burst step with
    // /// the step `index`; all copies share the same sequence number and it
    // /// only advances after the last copy (`index + 1 == repeats`).
    // ///
    // /// Intended for duty-cycled neighbours: a flood spans `repeats * step`
    // /// in time (the main loop paces the steps), so a node that only listens
    // /// for part of its wake window still catches at least one copy — while
    // /// seeing exactly one distinct OGM for deduplication and routing.
    // /// Queuing one copy per call (as the sender ping burst does) keeps heap
    // /// usage bounded.
    // pub fn send_ogm_burst(&mut self, index: u32, repeats: u32) {
    //     let message = Message::Ogm {
    //         source: self
    //             .hardware_interface
    //             .as_ref()
    //             .unwrap()
    //             .lock()
    //             .get_uwb_address(),
    //         sequence_number: self.next_ogm_sequence_number,
    //     };
    //     let data = message.to_data();
    //     let mut hi = self.hardware_interface.as_ref().unwrap().lock();
    //     let time_until_send_in_ms = hi.generate_random_integer() % 2 * 10;

    //     let broadcast_address =
    //         hardware_interface::UwbAddress::new(0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff);
    //     self.outgoing_message_handler.add_message(OutgoingMessage {
    //         destination: broadcast_address,
    //         data,
    //         time_to_send_in_ms: time_until_send_in_ms as i32,
    //     });
    //     drop(hi);

    //     if index + 1 == repeats {
    //         self.next_ogm_sequence_number = self.next_ogm_sequence_number.wrapping_add(1);
    //     }
    // }

    /// Forwards an OGM (Originator Message) to the specified destination, preserving the source and sequence number.
    fn forward_ogm(
        &mut self,
        destination: &hardware_interface::UwbAddress,
        source: hardware_interface::UwbAddress,
        sequence_number: u32,
    ) {
        let message = Message::Ogm {
            source,
            sequence_number,
        };
        let mut hi = self.hardware_interface.as_ref().unwrap().lock();
        let mut time_until_send_in_ms = 0;
        let message_data = Rc::new(RefCell::new(message.to_data()));
        for _ in 0..self.ogm_burst_number {
            time_until_send_in_ms += hi.generate_random_integer() % 2 * 10;

            let outgoing_message = OutgoingMessage {
                destination: *destination,
                data: Rc::clone(&message_data),
                time_to_send_in_ms: time_until_send_in_ms as i32,
            };
            self.outgoing_message_handler.add_message(outgoing_message);
        }
    }

    /// Ages the received OGMs by the specified time in milliseconds.
    fn age_received_ogms(&mut self, time_passed_in_ms: u32) {
        for (_, age) in self.received_ogm_to_age_in_ms.iter_mut() {
            *age = age.saturating_add(time_passed_in_ms);
        }
    }

    /// Gets the current routing table, which maps destination addresses to the next hop address.
    pub fn get_routing_table(
        &self,
    ) -> ArrayMap<hardware_interface::UwbAddress, hardware_interface::UwbAddress, 4> {
        self.routing_table
    }

    /// Drains one addressed-to-me unicast payload delivered by the mesh, if
    /// any. The application owns the acknowledgement/retransmission logic.
    pub fn take_received_data(&mut self) -> Option<(hardware_interface::UwbAddress, Vec<u8>)> {
        if self.received_data.is_empty() {
            return None;
        }
        Some(self.received_data.remove(0))
    }

    /// Sends a unicast message to the specified destination with the given data.
    pub fn send_unicast(&mut self, destination: hardware_interface::UwbAddress, data: Vec<u8>) {
        let own_address = self
            .hardware_interface
            .as_ref()
            .unwrap()
            .lock()
            .get_uwb_address();

        let message = Message::Unicast {
            source: own_address,
            previous_hop: own_address,
            destination,
            sequence_number: self.next_unicast_sequence_number,
            data,
        };
        let data = Rc::new(RefCell::new(message.to_data()));
        let mut hi = self.hardware_interface.as_ref().unwrap().lock();
        let time_until_send_in_ms = hi.generate_random_integer() % 2 * 10;

        let immediate_destination = if let Some(next_hop) = self.routing_table.get(&destination) {
            *next_hop
        } else {
            destination
        };

        let outgoing_message = OutgoingMessage {
            destination: immediate_destination,
            data,
            time_to_send_in_ms: time_until_send_in_ms as i32,
        };
        self.outgoing_message_retry_handler
            .add_message(outgoing_message, self.next_unicast_sequence_number);

        self.next_unicast_sequence_number = self.next_unicast_sequence_number.wrapping_add(1);
    }
}
