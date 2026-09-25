use std::time::Duration;

use crate::protocol::cbw_csw::AicCsw;

/// Policy for the trailing CSW of a transaction.
///
/// During the updater stage the device may reset before it can answer the
/// final command, so callers can opt into tolerating a missing CSW.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CswPolicy {
    Required,
    AllowMissing,
}

/// Transport abstraction for the ArtInChip UPG protocol.
///
/// Implementations only need to provide the CBW/CSW transaction primitives;
/// all UPG command and burn logic lives in [`crate::device::UpgDevice`].
pub trait UpgTransport {
    /// Full write transaction: CBW -> data -> CSW.
    fn write_txn(&mut self, payload: &[u8], policy: CswPolicy) -> Result<Option<AicCsw>, String>;

    /// Full read transaction: CBW -> data -> CSW. Returns the data payload.
    fn read_txn(&mut self, read_len: u32, policy: CswPolicy) -> Result<Vec<u8>, String>;

    /// Wait until the device is reachable again after a reset/reboot.
    fn reconnect(&mut self, timeout: Duration) -> Result<(), String>;

    /// Discard stale inbound bytes (used when a CSW went missing).
    fn drain_rx(&mut self, _timeout: Duration, _max_bytes: usize) -> Result<(), String> {
        Ok(())
    }

    /// Human readable transport name for logs.
    fn transport_name(&self) -> &'static str;

    /// Maximum payload size for a single write transaction.
    ///
    /// UART limits this because the device slices transfers into 64 KiB
    /// buffers and uses a small RX ring buffer with stop-and-wait framing.
    fn max_write_chunk(&self, _block_size: u32) -> usize {
        usize::MAX
    }

    /// Negotiate a higher UART baudrate (`SET_UART_ARGS`) when supported.
    fn set_uart_baudrate(&mut self, _baudrate: u32) -> Result<(), String> {
        Err("this transport does not support UART baudrate negotiation".to_string())
    }
}
