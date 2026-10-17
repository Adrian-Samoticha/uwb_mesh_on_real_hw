Implementation of https://github.com/Adrian-Samoticha/uwb_mesh on actual hardware.

See `how_to_run.txt` for instructions on how to run this project.
The `base-station` is a node that regularly broadcasts OGM messages across the network.
The `sender` is a node that regularly sends unicast messages to the `base-station`.
The frequencies are defined in `main.rs` by the following lines of code:
```rust
/// Base-station: OGM broadcast interval.
#[cfg(feature = "base-station")]
const BASE_STATION_OGM_INTERVAL: Duration = Duration::from_secs(10);
/// Sender: ping interval.
#[cfg(not(feature = "base-station"))]
const SENDER_PING_INTERVAL: Duration = Duration::from_secs(10);
```

Currently, the addresses of the `base-station` and `sender` are hardcoded as follows in `main.rs`:
```rust
/// Short address of the base-station board.
#[cfg(feature = "base-station")]
const BASE_STATION_ADDRESS: [u8; 2] = [0xAA, 0x01];
/// Short address of the sender board.
#[cfg(not(feature = "base-station"))]
const SENDER_ADDRESS: [u8; 2] = [0xAA, 0x02];
```

To support meshes with more than two devices, a method to provide unique addresses to senders needs to be implemented.
