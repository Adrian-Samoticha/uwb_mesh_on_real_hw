extern crate alloc;
use alloc::sync::Arc;
use spin::Mutex;
use std::sync::Weak;
use std::sync::atomic::{AtomicI32, Ordering};

pub type Id = u64;

use crate::interfaces::hardware_interface_simulator_impl::HardwareInterfaceSimulatorImpl;
use crate::simulator::SimulatorMutex;

mod array_map;
mod firmware;
mod mesh;

pub use array_map::ArrayMap;

pub struct Device {
    id: Id,
    middleware: parking_lot::Mutex<Option<firmware::Firmware>>,
    heartbeats_left: AtomicI32,
}

impl Device {
    pub fn new(id: Id) -> Self {
        Self {
            id,
            middleware: parking_lot::Mutex::new(None),
            heartbeats_left: AtomicI32::new(0),
        }
    }

    pub fn create() -> Arc<Self> {
        let new_id = crate::id_provider::get_next_id();
        Arc::new(Self::new(new_id))
    }

    pub fn init(self: &Arc<Self>, name: &str, model: &str, simulator: Weak<SimulatorMutex>) {
        let mut firmware = firmware::Firmware::new();

        let hardware_interface = HardwareInterfaceSimulatorImpl::new(simulator, self.id as u32);

        firmware.init(name, model, Arc::new(Mutex::new(hardware_interface)));

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
