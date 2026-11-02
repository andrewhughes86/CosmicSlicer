//! A minimal MQTT 3.1.1 client (connect, subscribe, publish at QoS 0, ping): what Bambu
//! printers' local broker needs. It works over any stream, so tests use a plain socket.

use std::io::{self, Read, Write};

/// Largest packet accepted (Bambu's full reports are tens of kilobytes).
const MAX_PACKET: usize = 4 << 20;

pub struct Client<S: Read + Write> {
    stream: S,
    next_id: u16,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let len = b.len().min(u16::MAX as usize);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(b.get(..len).unwrap_or(b));
}

/// A packet: its type byte, then the remaining length as a varint, then `body`.
fn packet(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    let mut len = body.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            break;
        }
    }
    out.extend_from_slice(body);
    out
}

fn timed_out(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

impl<S: Read + Write> Client<S> {
    /// Logs in on an open stream; fails if the broker refuses.
    pub fn connect(mut stream: S, client_id: &str, user: &str, password: &str, keep_alive_s: u16) -> io::Result<Self> {
        let mut body = Vec::new();
        put_str(&mut body, "MQTT");
        body.push(4);
        body.push(0x80 | 0x40 | 0x02);
        body.extend_from_slice(&keep_alive_s.to_be_bytes());
        put_str(&mut body, client_id);
        put_str(&mut body, user);
        put_str(&mut body, password);
        stream.write_all(&packet(0x10, &body))?;
        stream.flush()?;
        let mut client = Client { stream, next_id: 1 };
        match client.read_packet()? {
            Some((0x20, b)) => match b.get(1) {
                Some(0) => Ok(client),
                Some(4 | 5) => Err(io::Error::other("the printer refused the access code")),
                Some(c) => Err(io::Error::other(format!("the printer refused the connection ({c})"))),
                None => Err(io::Error::other("a broken reply from the printer")),
            },
            _ => Err(io::Error::other("the printer did not accept the connection")),
        }
    }

    pub fn subscribe(&mut self, topic: &str) -> io::Result<()> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut body = id.to_be_bytes().to_vec();
        put_str(&mut body, topic);
        body.push(0);
        self.send(&packet(0x82, &body))
    }

    pub fn publish(&mut self, topic: &str, payload: &[u8]) -> io::Result<()> {
        let mut body = Vec::new();
        put_str(&mut body, topic);
        body.extend_from_slice(payload);
        self.send(&packet(0x30, &body))
    }

    pub fn ping(&mut self) -> io::Result<()> {
        self.send(&[0xC0, 0])
    }

    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    /// The next message published to us, `None` when the read times out with nothing; other
    /// packets (acknowledgements, ping replies) are passed over.
    pub fn poll(&mut self) -> io::Result<Option<(String, Vec<u8>)>> {
        loop {
            let Some((kind, body)) = self.read_packet()? else { return Ok(None) };
            if kind & 0xF0 != 0x30 {
                continue;
            }
            let len = match body.get(..2) {
                Some(&[a, b]) => u16::from_be_bytes([a, b]) as usize,
                _ => continue,
            };
            let Some(topic) = body.get(2..2 + len) else { continue };
            let topic = String::from_utf8_lossy(topic).into_owned();
            // QoS 1 and 2 carry a packet id after the topic.
            let skip = if kind & 0x06 != 0 { 2 } else { 0 };
            let payload = body.get(2 + len + skip..).unwrap_or(&[]).to_vec();
            return Ok(Some((topic, payload)));
        }
    }

    /// One packet: (type byte, body); `None` if nothing arrived before the read timeout.
    fn read_packet(&mut self) -> io::Result<Option<(u8, Vec<u8>)>> {
        let mut first = [0u8; 1];
        match self.stream.read(&mut first) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the printer closed the connection")),
            Ok(_) => {}
            Err(e) if timed_out(&e) => return Ok(None),
            Err(e) => return Err(e),
        }
        let mut len = 0usize;
        let mut shift = 0;
        for _ in 0..4 {
            let mut b = [0u8; 1];
            self.stream.read_exact(&mut b)?;
            len |= ((b[0] & 0x7F) as usize) << shift;
            shift += 7;
            if b[0] & 0x80 == 0 {
                break;
            }
        }
        if len > MAX_PACKET {
            return Err(io::Error::other("a message from the printer is too large"));
        }
        let mut body = vec![0u8; len];
        self.stream.read_exact(&mut body)?;
        Ok(Some((first[0], body)))
    }

    pub fn stream(&self) -> &S {
        &self.stream
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    /// A fake broker: accepts one client, acknowledges it, then hands the socket to `run`.
    pub fn broker(run: impl FnOnce(TcpStream, Vec<u8>) + Send + 'static) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").ok();
        let port = l.as_ref().and_then(|l| l.local_addr().ok()).map_or(0, |a| a.port());
        std::thread::spawn(move || {
            let Some(l) = l else { return };
            let Ok((mut s, _)) = l.accept() else { return };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut head = [0u8; 2];
            if s.read_exact(&mut head).is_err() {
                return;
            }
            let mut connect = vec![0u8; head[1] as usize];
            let _ = s.read_exact(&mut connect);
            let _ = s.write_all(&[0x20, 2, 0, 0]);
            run(s, connect);
        });
        port
    }

    #[test]
    fn talks_to_a_broker() -> io::Result<()> {
        let port = broker(|mut s, connect| {
            let text = String::from_utf8_lossy(&connect).into_owned();
            assert!(text.contains("bblp") && text.contains("12345678"));
            // Read the subscription, then publish a report.
            let mut buf = [0u8; 64];
            let _ = s.read(&mut buf);
            let _ = s.write_all(&[0x90, 3, 0, 1, 0]);
            let _ = s.write_all(&packet(0x30, &{
                let mut b = Vec::new();
                put_str(&mut b, "device/X/report");
                b.extend_from_slice(br#"{"print":{"mc_percent":5}}"#);
                b
            }));
            std::thread::sleep(Duration::from_millis(300));
        });
        let s = TcpStream::connect(("127.0.0.1", port))?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut c = Client::connect(s, "cs", "bblp", "12345678", 30)?;
        c.subscribe("device/X/report")?;
        let got = c.poll()?;
        assert_eq!(
            got.map(|(t, p)| (t, String::from_utf8_lossy(&p).into_owned())),
            Some(("device/X/report".into(), r#"{"print":{"mc_percent":5}}"#.into()))
        );
        Ok(())
    }

    #[test]
    fn varint_lengths() {
        assert_eq!(packet(0x30, &[0; 200]).get(1..3), Some(&[0xC8, 0x01][..]));
    }
}
