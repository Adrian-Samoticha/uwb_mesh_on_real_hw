extern crate alloc;
use alloc::sync::Arc;
use alloc::{string::String, string::ToString, vec::Vec};
use spin::Mutex;

use crate::{
    device::mesh,
    interfaces::hardware_interface::{self},
};

pub struct Firmware {
    name: String,
    model: String,
    hardware_interface: Option<Arc<Mutex<dyn hardware_interface::HardwareInterface>>>,
    mesh: mesh::Mesh,
    serial_buffer: Vec<u8>,
}

impl Firmware {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            model: String::new(),
            hardware_interface: None,
            mesh: mesh::Mesh::new(),
            serial_buffer: Vec::new(),
        }
    }

    pub fn init(
        &mut self,
        name: &str,
        model: &str,
        hardware_interface: Arc<Mutex<dyn hardware_interface::HardwareInterface>>,
    ) {
        self.name = name.to_string();
        self.model = model.to_string();
        self.hardware_interface = Some(Arc::clone(&hardware_interface));

        let hi = self.hardware_interface.as_mut().unwrap();

        hi.lock().printf(format_args!("Hello World!\n"));

        self.mesh.init(hardware_interface);
    }

    pub fn heartbeat(&mut self) {
        self.mesh.heartbeat();

        self.handle_serial_input();
    }

    /// Flood an OGM so other nodes learn a route to us. Used by the
    /// real-hardware main loop (no serial console there).
    pub fn send_ogm(&mut self) {
        self.mesh.send_ogm();
    }

    // TODO: remove this (if it’s unused)
    // /// Queue one copy of a single-OGM flood (one sequence number); the main
    // /// loop calls this once per flood step with the step `index`.
    // pub fn send_ogm_burst(&mut self, index: u32, repeats: u32) {
    //     self.mesh.send_ogm_burst(index, repeats);
    // }

    /// Queue a unicast frame for `destination`. Used by the real-hardware
    /// main loop (no serial console there).
    pub fn send_unicast(&mut self, destination: hardware_interface::UwbAddress, data: Vec<u8>) {
        self.mesh.send_unicast(destination, data);
    }

    /// Drain one addressed-to-me unicast payload (source, data) received by
    /// the mesh; application-level acknowledgement logic lives elsewhere.
    pub fn take_received_data(&mut self) -> Option<(hardware_interface::UwbAddress, Vec<u8>)> {
        self.mesh.take_received_data()
    }

    /// Snapshot of the mesh routing table (destination -> next hop).
    pub fn get_routing_table(
        &self,
    ) -> crate::device::ArrayMap<hardware_interface::UwbAddress, hardware_interface::UwbAddress, 4>
    {
        self.mesh.get_routing_table()
    }

    fn handle_serial_input(&mut self) {
        let chars: Vec<char> = {
            if let Some(hi) = &self.hardware_interface {
                let hi = hi.lock();
                let mut chars = Vec::new();
                while hi.is_serial_input_available() {
                    if let Some(c) = hi.read_char_over_serial() {
                        chars.push(c);
                    }
                }
                chars
            } else {
                Vec::new()
            }
        };

        for c in chars {
            if c == ';' {
                let command = String::from_utf8_lossy(&self.serial_buffer).to_string();
                self.execute_serial_command(&command);
                self.serial_buffer.clear();
            } else {
                self.serial_buffer.push(c as u8);
            }
        }
    }

    fn parse_address(&self, address_str: &str) -> Option<hardware_interface::UwbAddress> {
        if address_str.starts_with("0x") {
            let addr = u16::from_str_radix(&address_str[2..], 16).ok()?;
            Some(hardware_interface::UwbAddress::Short(addr))
        } else {
            let separator = ':';
            let parts: Vec<&str> = address_str.split(separator).collect();
            if parts.len() != 8 {
                return Some(hardware_interface::UwbAddress::Short(0));
            }
            let mut addr = [0u8; 8];
            for i in 0..8 {
                addr[i] = u8::from_str_radix(parts[i], 16).unwrap_or(0);
            }
            Some(hardware_interface::UwbAddress::Extended(addr))
        }
    }

    fn execute_serial_command(&mut self, command: &str) {
        let command = command.trim();
        let parts: Vec<&str> = command.split_whitespace().collect();
        let cmd_name = parts.get(0).map(|s| *s).unwrap_or("");

        match cmd_name {
            "send_ogm" => {
                self.mesh.send_ogm();

                let hi = self.hardware_interface.as_mut().unwrap();
                hi.lock().printf(format_args!("Sent OGM\n"));
            }
            "print_rt" => {
                let routing_table = self.mesh.get_routing_table();
                let hi = self.hardware_interface.as_mut().unwrap();
                hi.lock().printf(format_args!("Routing Table:\n"));
                for (dest, next_hop) in routing_table.iter() {
                    hi.lock().printf(format_args!(
                        "Destination: {:?}, Next Hop: {:?}\n",
                        dest, next_hop
                    ));
                }
            }
            "send_unicast" => {
                if parts.len() < 3 {
                    let hi = self.hardware_interface.as_mut().unwrap();
                    hi.lock().printf(format_args!(
                        "Usage: send_unicast <destination> <message>\n"
                    ));
                    return;
                }
                let destination_str = parts[1];
                let message = parts[2..].join(" ");

                let destination = self.parse_address(destination_str);
                if destination.is_none() {
                    let hi = self.hardware_interface.as_mut().unwrap();
                    hi.lock().printf(format_args!(
                        "Invalid destination address: {}\n",
                        destination_str
                    ));
                    return;
                }

                self.mesh
                    .send_unicast(destination.unwrap(), message.as_bytes().to_vec());

                let hint = self.hardware_interface.as_mut().unwrap();
                hint.lock().printf(format_args!(
                    "Sent unicast message to {}: {}\n",
                    destination_str, message
                ));
            }
            "print_uwb_address" => {
                let hi = self.hardware_interface.as_mut().unwrap();
                let address = hi.lock().get_uwb_address();
                hi.lock()
                    .printf(format_args!("UWB Address: {:?}\n", address));
            }
            _ => {
                let hi = self.hardware_interface.as_mut().unwrap();
                hi.lock()
                    .printf(format_args!("Unknown command: {}\n", command));
            }
        }
    }
}
