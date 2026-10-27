//! Lines of a byte stream.

use iroh::endpoint::RecvStream;

#[derive(Default)]
pub struct Lines(Vec<u8>);

impl Lines {
    pub fn next(&mut self) -> Option<String> {
        let end = self.0.iter().position(|&b| b == b'\n')?;
        let line: Vec<u8> = self.0.drain(..=end).collect();
        Some(String::from_utf8_lossy(&line).into_owned())
    }

    /// Reads more; false at the end of the stream.
    pub async fn fill(&mut self, recv: &mut RecvStream) -> Result<bool, String> {
        let mut buf = [0u8; 16 * 1024];
        match recv.read(&mut buf).await {
            Ok(Some(n)) => {
                if self.0.len() + n > 4 << 20 {
                    return Err("line too long".into());
                }
                self.0.extend_from_slice(&buf[..n]);
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The next line, or `None` at the end of the stream. Cancel-safe.
    pub async fn read_line(&mut self, recv: &mut RecvStream) -> Result<Option<String>, String> {
        loop {
            if let Some(l) = self.next() {
                return Ok(Some(l));
            }
            if !self.fill(recv).await? {
                return Ok(None);
            }
        }
    }
}
