use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::Duration;

const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RESPONSE_BYTES: u64 = 4 * 1024;
const REQUEST: &[u8] = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

fn response_is_healthy(response: &[u8]) -> bool {
    let Some(headers_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let Some(status_end) = response.windows(2).position(|window| window == b"\r\n") else {
        return false;
    };
    let status = &response[..status_end];
    (status.starts_with(b"HTTP/1.1 200 ") || status.starts_with(b"HTTP/1.0 200 "))
        && response.get(headers_end + 4..) == Some(b"ok".as_slice())
}

fn probe(address: SocketAddr) -> Result<bool, String> {
    let mut stream = TcpStream::connect_timeout(&address, HEALTHCHECK_TIMEOUT)
        .map_err(|error| format!("health endpoint connection failed: {error}"))?;
    stream
        .set_read_timeout(Some(HEALTHCHECK_TIMEOUT))
        .map_err(|error| format!("health endpoint read timeout setup failed: {error}"))?;
    stream
        .set_write_timeout(Some(HEALTHCHECK_TIMEOUT))
        .map_err(|error| format!("health endpoint write timeout setup failed: {error}"))?;
    stream
        .write_all(REQUEST)
        .map_err(|error| format!("health endpoint request failed: {error}"))?;

    let mut response = Vec::with_capacity(512);
    (&mut stream)
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)
        .map_err(|error| format!("health endpoint response failed: {error}"))?;
    if response.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("health endpoint response exceeded 4096 bytes".to_string());
    }
    Ok(response_is_healthy(&response))
}

pub(super) fn run<I>(mut arguments: I) -> i32
where
    I: Iterator<Item = String>,
{
    if let Some(argument) = arguments.next() {
        eprintln!("unexpected healthcheck argument {argument:?}");
        return 2;
    }
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080));
    match probe(address) {
        Ok(true) => 0,
        Ok(false) => {
            eprintln!("health endpoint did not return HTTP 200 with body ok");
            1
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_exact_health_contract() {
        assert!(response_is_healthy(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
        ));
        assert!(response_is_healthy(
            b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok"
        ));
        assert!(!response_is_healthy(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\n\r\nok"
        ));
        assert!(!response_is_healthy(
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nok\n"
        ));
        assert!(!response_is_healthy(b"HTTP/1.1 200 OK\r\n\r\n"));
    }

    #[test]
    fn rejects_arguments_without_starting_the_server() {
        assert_eq!(run(["unexpected".to_string()].into_iter()), 2);
    }
}
