use std::io::Write;
use std::time::Duration;

use anyhow::Context;
use serialport::SerialPort;

/// Espressif USB Vendor ID used on all Boppo devices.
const BOPPO_USB_VID: u16 = 0x303A;

/// An open USB serial connection to a Boppo device (TinyUSB CDC ACM).
pub struct BoppoUsbPort {
    port: Box<dyn SerialPort>,
}

impl BoppoUsbPort {
    /// Open the serial port at `path`.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let port = serialport::new(path, 115200)
            .timeout(Duration::from_millis(500))
            .open()
            .with_context(|| format!("failed to open serial port {}", path))?;
        Ok(Self { port })
    }

    /// Send a command line and collect the response.
    ///
    /// Blocks until ~500ms of silence after the last byte received.
    pub fn run_command(&mut self, command: &str) -> anyhow::Result<String> {
        let line = format!("{}\n", command);
        self.port
            .write_all(line.as_bytes())
            .context("failed to write to serial port")?;
        let _ = self.port.flush();
        read_response(&mut *self.port)
    }

    /// Send Wi-Fi credentials using the `add_wifi_network` serial command.
    pub fn send_wifi_credentials(
        &mut self,
        ssid: &str,
        password: Option<&str>,
    ) -> anyhow::Result<()> {
        let line = match password {
            Some(pw) if !pw.is_empty() => format!("add_wifi_network '{}' '{}'\n", ssid, pw),
            _ => format!("add_wifi_network '{}'\n", ssid),
        };
        self.port
            .write_all(line.as_bytes())
            .context("failed to write to serial port")?;
        let _ = self.port.flush();
        Ok(())
    }
}

/// Search connected serial ports for a Boppo device and return the port path.
///
/// Matches on the Espressif VID (`0x303A`) plus a manufacturer or product
/// string containing "Boppo".
pub fn find_boppo_port() -> anyhow::Result<Option<String>> {
    let ports = serialport::available_ports().context("failed to enumerate serial ports")?;
    for port in ports {
        if let serialport::SerialPortType::UsbPort(info) = &port.port_type {
            if info.vid == BOPPO_USB_VID {
                let is_boppo = info
                    .manufacturer
                    .as_deref()
                    .is_some_and(|m| m.contains("Boppo"))
                    || info
                        .product
                        .as_deref()
                        .is_some_and(|p| p.contains("Boppo"));
                if is_boppo {
                    return Ok(Some(port.port_name));
                }
            }
        }
    }
    Ok(None)
}

/// Read bytes from `port` until a read times out (~500ms of silence).
fn read_response(port: &mut dyn SerialPort) -> anyhow::Result<String> {
    let mut output = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        match port.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(e).context("error reading serial response"),
        }
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}
