pub mod monitor;
pub mod transport;

pub use monitor::{run_cli_monitor, MonitorCommand, UartMonitor};
pub use transport::{SerialPortInfo, UartOptions, UartTransport};

use crate::device::UpgDevice;

/// UART bound device.
pub type UartDevice = UpgDevice<UartTransport>;
