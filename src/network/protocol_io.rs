use quinn::{RecvStream, SendStream};

use crate::error::Result;

use super::{MAX_MESSAGE_SIZE, protocol::WireMessage, util::other, util::other_message};

pub(super) async fn write_message(send: &mut SendStream, message: &WireMessage) -> Result<()> {
    let payload =
        serde_json::to_vec(message).map_err(|error| other("encode network message", error))?;
    if payload.len() > MAX_MESSAGE_SIZE {
        return Err(other_message(
            "encode network message",
            "message is too large",
        ));
    }
    send.write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .map_err(|error| other("write network message length", error))?;
    send.write_all(&payload)
        .await
        .map_err(|error| other("write network message payload", error))?;
    Ok(())
}

pub(super) async fn read_message(recv: &mut RecvStream) -> Result<WireMessage> {
    let mut len = [0_u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|error| other("read network message length", error))?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(other_message(
            "read network message",
            "message is too large",
        ));
    }
    let mut payload = vec![0_u8; len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|error| other("read network message payload", error))?;
    serde_json::from_slice(&payload).map_err(|error| other("decode network message", error))
}
