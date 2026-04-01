use engine::Backend;
use engine::logs::Logs;

/// Unmanaged debug connection over two serial ports (USART1 + USART2).
pub struct Serial {
    logs: SerialLogs,
}

struct SerialLogs {
    structured_tx: tokio::sync::broadcast::Sender<engine::logs::LogEntry>,
    hypervisor_tx: tokio::sync::broadcast::Sender<engine::logs::HypervisorLine>,
}

impl Serial {
    /// Connect to a device over two serial ports.
    ///
    /// - `usart1`: hypervisor/supervisor text stream (1M baud)
    /// - `usart2`: structured binary log stream (115200 baud)
    pub fn connect(
        _usart1: &str,
        _usart2: &str,
    ) -> Result<Self, serialport::Error> {
        let (structured_tx, _) = tokio::sync::broadcast::channel(256);
        let (hypervisor_tx, _) = tokio::sync::broadcast::channel(256);

        // TODO: open serial ports, spawn reader tasks

        Ok(Serial {
            logs: SerialLogs {
                structured_tx,
                hypervisor_tx,
            },
        })
    }
}

impl Backend for Serial {
    fn logs(&self) -> &dyn Logs {
        &self.logs
    }
}

impl Logs for SerialLogs {
    fn subscribe_structured(&self) -> tokio::sync::broadcast::Receiver<engine::logs::LogEntry> {
        self.structured_tx.subscribe()
    }

    fn subscribe_hypervisor(&self) -> tokio::sync::broadcast::Receiver<engine::logs::HypervisorLine> {
        self.hypervisor_tx.subscribe()
    }
}
