//! Who is on the other end of a connection to the service. Linux only: its
//! socket table names the owner of both ends of a loopback connection.

use std::net::{SocketAddr, SocketAddrV4};

use tokio::net::TcpStream;

const SOCKET_TABLE: &str = "/proc/net/tcp";

/// Whether the program that connected runs as the same user as Irori. Any doubt
/// is a no: the port hands out tokens to whoever is allowed through.
pub(crate) async fn same_user(socket: &TcpStream) -> bool {
    let (Ok(SocketAddr::V4(service)), Ok(SocketAddr::V4(peer))) =
        (socket.local_addr(), socket.peer_addr())
    else {
        return false;
    };
    match tokio::fs::read_to_string(SOCKET_TABLE).await {
        Ok(table) => same_owner(&table, service, peer),
        Err(e) => {
            tracing::warn!("Galaxy service: reading {SOCKET_TABLE}: {e}");
            false
        }
    }
}

fn same_owner(table: &str, service: SocketAddrV4, peer: SocketAddrV4) -> bool {
    let ours = socket_owner(table, service, peer);
    ours.is_some() && ours == socket_owner(table, peer, service)
}

/// Owner of the socket at `local` connected to `remote`, from a table with the
/// layout `sl local_address rem_address st tx:rx tr:when retrnsmt uid ...`.
fn socket_owner(table: &str, local: SocketAddrV4, remote: SocketAddrV4) -> Option<u32> {
    let (local, remote) = (table_address(local), table_address(remote));
    table.lines().skip(1).find_map(|line| {
        let mut fields = line.split_whitespace().skip(1);
        let matches = fields.next()? == local && fields.next()? == remote;
        matches.then(|| fields.nth(4)?.parse().ok()).flatten()
    })
}

/// The table prints an address as the bytes of its in-memory value, so its
/// order follows the machine's.
fn table_address(address: SocketAddrV4) -> String {
    let ip = u32::from_ne_bytes(address.ip().octets());
    format!("{ip:08X}:{:04X}", address.port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;

    fn loopback(port: u16) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
    }

    fn table(service_uid: u32, peer_uid: u32) -> String {
        let service = table_address(loopback(9977));
        let peer = table_address(loopback(40000));
        format!(
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
             \x20  0: {service} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11 1 0 100 0 0 10 0\n\
             \x20  1: {service} {peer} 01 00000000:00000000 00:00000000 00000000  {service_uid}        0 12 1 0 20 4 30 10 -1\n\
             \x20  2: {peer} {service} 01 00000000:00000000 00:00000000 00000000  {peer_uid}        0 13 1 0 20 4 30 10 -1\n"
        )
    }

    #[test]
    fn accepts_only_the_same_owner() {
        let (service, peer) = (loopback(9977), loopback(40000));
        assert!(same_owner(&table(1000, 1000), service, peer));
        assert!(!same_owner(&table(1000, 1001), service, peer));
        assert!(!same_owner(&table(1000, 0), service, peer));
    }

    #[test]
    fn a_peer_missing_from_the_table_is_refused() {
        let (service, peer) = (loopback(9977), loopback(40000));
        assert!(!same_owner(&table(1000, 1000), service, loopback(40001)));
        assert!(!same_owner("header only\n", service, peer));
        assert!(!same_owner("", service, peer));
    }

    #[tokio::test]
    async fn recognizes_a_connection_from_this_process() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let _client = TcpStream::connect(address).await.unwrap();
        let (socket, _) = listener.accept().await.unwrap();
        assert!(same_user(&socket).await);
    }
}
