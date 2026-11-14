//! Frame codec, wire-compatible with happd: u32 big-endian length + UTF-8 JSON.
use std::io::{self, Read, Write};

pub const MAX_FRAME: u32 = 64 * 1024 * 1024;

pub fn write_frame<W: Write>(w: &mut W, json: &serde_json::Value) -> io::Result<()> {
    let body = serde_json::to_vec(json)?;
    let len = body.len() as u32;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<serde_json::Value>> {
    let mut hdr = [0u8; 4];
    match r.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(hdr);
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(v))
}
