use crate::models::ServerInfo;
use serde::Deserialize;
use std::net::UdpSocket;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct JellyfinBroadcastResponse {
    #[serde(rename = "Address")]
    pub address: Option<String>,
    #[serde(rename = "Id")]
    pub id: Option<String>,
    #[serde(rename = "Name")]
    pub name: Option<String>,
    #[serde(rename = "EndpointAddress")]
    pub endpoint_address: Option<String>,
}

pub const JELLYFIN_DISCOVERY_MESSAGE: &[u8] = b"who is JellyfinServer?";
pub const JELLYFIN_DISCOVERY_PORT: u16 = 7359;

/// Discovers Jellyfin servers on the local network via UDP broadcast to 255.255.255.255:7359.
pub fn discover_servers(timeout: Duration) -> Vec<ServerInfo> {
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => {
            return Vec::new();
        }
    };

    let _ = socket.set_broadcast(true);
    let target = format!("255.255.255.255:{JELLYFIN_DISCOVERY_PORT}");
    discover_servers_with_socket(&socket, &target, timeout)
}

/// Helper function taking an existing socket and target address (useful for testing).
pub fn discover_servers_with_socket(
    socket: &UdpSocket,
    target: &str,
    timeout: Duration,
) -> Vec<ServerInfo> {
    let mut servers = Vec::new();

    if socket.send_to(JELLYFIN_DISCOVERY_MESSAGE, target).is_err() {
        return servers;
    }

    let mut buf = [0u8; 4096];
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        let read_slice = remaining.min(Duration::from_millis(400));
        let _ = socket.set_read_timeout(Some(read_slice));

        match socket.recv_from(&mut buf) {
            Ok((len, peer)) => {
                if let Ok(text) = std::str::from_utf8(&buf[..len]) {
                    if let Ok(dto) = serde_json::from_str::<JellyfinBroadcastResponse>(text) {
                        let id = dto.id.unwrap_or_default();
                        let name = dto.name.unwrap_or_else(|| "Jellyfin Server".into());
                        let raw_url = dto
                            .address
                            .or(dto.endpoint_address)
                            .unwrap_or_else(|| format!("http://{}:8096", peer.ip()));

                        let exists = servers
                            .iter()
                            .any(|s: &ServerInfo| (!id.is_empty() && s.id == id) || s.url == raw_url);
                        if !exists {
                            servers.push(ServerInfo {
                                id,
                                name,
                                version: String::new(),
                                url: raw_url,
                            });
                        }
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Continue loop until overall timeout
            }
            Err(_) => {
                break;
            }
        }
    }

    servers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_broadcast_response() {
        let json = r#"{"Address":"http://192.168.1.111:8096","Id":"8be2a082dd814cec8ae83ea4cfcb83bf","Name":"wtr","EndpointAddress":null}"#;
        let dto: JellyfinBroadcastResponse = serde_json::from_str(json).unwrap();
        assert_eq!(dto.address.as_deref(), Some("http://192.168.1.111:8096"));
        assert_eq!(dto.id.as_deref(), Some("8be2a082dd814cec8ae83ea4cfcb83bf"));
        assert_eq!(dto.name.as_deref(), Some("wtr"));
    }

    #[test]
    fn mock_udp_discovery_exchange() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server_sock.local_addr().unwrap();

        let client_sock = UdpSocket::bind("127.0.0.1:0").unwrap();

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (len, peer) = server_sock.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..len], JELLYFIN_DISCOVERY_MESSAGE);
            let resp = br#"{"Address":"http://127.0.0.1:8096","Id":"mock-id","Name":"MockServer"}"#;
            server_sock.send_to(resp, peer).unwrap();
        });

        let servers = discover_servers_with_socket(
            &client_sock,
            &server_addr.to_string(),
            Duration::from_millis(1500),
        );
        handle.join().unwrap();

        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "MockServer");
        assert_eq!(servers[0].url, "http://127.0.0.1:8096");
        assert_eq!(servers[0].id, "mock-id");
    }

    #[test]
    fn discovers_local_server_if_available() {
        let _ = discover_servers(Duration::from_millis(500));
    }
}
