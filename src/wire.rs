use crate::error::{ProtocolError, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

#[cfg(feature = "msgpack")]
pub fn encode_msgpack<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    rmp_serde::to_vec(value).map_err(|e| ProtocolError::Encoding(e.to_string()))
}

#[cfg(feature = "msgpack")]
pub fn decode_msgpack<'a, T: serde::de::DeserializeOwned>(data: &'a [u8]) -> Result<T> {
    rmp_serde::from_slice(data).map_err(|e| ProtocolError::Encoding(e.to_string()))
}

pub async fn write_message<W>(w: &mut W, msg_type: u8, payload: &[u8]) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let len = 2 + payload.len();
    if len > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::InvalidMessage("message too large".into()));
    }

    let len_bytes = (len as u32).to_be_bytes();
    w.write_all(&len_bytes).await?;
    w.write_all(&[msg_type]).await?;
    w.write_all(&[1]).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_message<R>(r: &mut R) -> Result<(u8, Vec<u8>)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len < 2 || len > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::InvalidMessage(format!(
            "invalid length: {}",
            len
        )));
    }

    let mut type_buf = [0u8; 1];
    r.read_exact(&mut type_buf).await?;
    let msg_type = type_buf[0];

    let mut version_buf = [0u8; 1];
    r.read_exact(&mut version_buf).await?;

    let payload_len = len - 2;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        r.read_exact(&mut payload).await?;
    }

    Ok((msg_type, payload))
}

pub const MSG_HANDSHAKE: u8 = 0x01;
pub const MSG_HANDSHAKE_ACK: u8 = 0x02;
pub const MSG_REQUEST: u8 = 0x10;
pub const MSG_RESPONSE: u8 = 0x11;
pub const MSG_STREAM_REQUEST: u8 = 0x12;
pub const MSG_STREAM_START: u8 = 0x13;
pub const MSG_STREAM_CHUNK: u8 = 0x14;
pub const MSG_STREAM_END: u8 = 0x15;
pub const MSG_ERROR: u8 = 0x1F;
pub const MSG_PING: u8 = 0x40;
pub const MSG_PONG: u8 = 0x41;
pub const MSG_CLOSE: u8 = 0x50;
pub const MSG_SCHEMA_REQUEST: u8 = 0x60;
pub const MSG_SCHEMA_RESPONSE: u8 = 0x61;
