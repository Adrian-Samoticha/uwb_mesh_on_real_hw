extern crate alloc;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, Ordering};
use spin::Mutex;

pub type Id = u64;

use crate::interfaces::hardware_interface;

mod array_map;
mod firmware;
mod mesh;

pub struct Device {
    id: Id,
    middleware: Mutex<Option<firmware::Firmware>>,
    heartbeats_left: AtomicI32,
}

#[allow(dead_code)]
impl Device {
    pub fn new(id: Id) -> Self {
        Self {
            id,
            middleware: Mutex::new(None),
            heartbeats_left: AtomicI32::new(0),
        }
    }

    pub fn create(id: Id) -> Arc<Self> {
        Arc::new(Self::new(id))
    }

    pub fn init(
        self: &Arc<Self>,
        name: &str,
        model: &str,
        hardware_interface: Arc<Mutex<dyn hardware_interface::HardwareInterface>>,
    ) {
        let mut firmware = firmware::Firmware::new();

        firmware.init(name, model, hardware_interface);

        let pending = self.heartbeats_left.swap(0, Ordering::Relaxed);
        for _ in 0..pending {
            firmware.heartbeat();
        }

        let mut guard = self.middleware.lock();
        *guard = Some(firmware);
    }

    pub fn heartbeat(&self) {
        let mut guard = self.middleware.lock();
        if let Some(middleware) = guard.as_mut() {
            middleware.heartbeat();
        } else {
            self.heartbeats_left.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Queue an OGM flood. No-op until `init` has installed the firmware.
    pub fn send_ogm(&self) {
        if let Some(middleware) = self.middleware.lock().as_mut() {
            middleware.send_ogm();
        }
    }

    /// Queue a unicast frame. No-op until `init` has installed the firmware.
    pub fn send_unicast(&self, destination: hardware_interface::UwbAddress, data: alloc::vec::Vec<u8>) {
        if let Some(middleware) = self.middleware.lock().as_mut() {
            middleware.send_unicast(destination, data);
        }
    }

    pub fn get_heartbeats_left(&self) -> i32 {
        self.heartbeats_left.load(Ordering::Relaxed)
    }

    pub fn destroy(&self) {
        let mut guard = self.middleware.lock();
        *guard = None;
    }

    pub fn get_id(&self) -> Id {
        self.id
    }
}
