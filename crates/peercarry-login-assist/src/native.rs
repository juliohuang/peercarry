//! Chrome native messaging uses a bounded length-prefixed JSON stream.
use serde_json::Value;
use std::io::{self, Read, Write};
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
pub fn read_message(mut input: impl Read) -> io::Result<Value> {
    let mut size = [0; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    if size == 0 || size > MAX_MESSAGE {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut buffer = zeroize::Zeroizing::new(vec![0; size]);
    input.read_exact(&mut buffer)?;
    serde_json::from_slice(&buffer).map_err(|_| io::ErrorKind::InvalidData.into())
}
pub fn write_message(mut output: impl Write, value: &Value) -> io::Result<()> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value)?);
    if bytes.len() > 1024 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    output.write_all(&(bytes.len() as u32).to_le_bytes())?;
    output.write_all(&bytes)?;
    output.flush()
}
