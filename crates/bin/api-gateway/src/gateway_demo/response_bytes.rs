use axum::body::Bytes;
use tokio::sync::OwnedSemaphorePermit;

struct BufferedResponse {
    body: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for BufferedResponse {
    fn as_ref(&self) -> &[u8] {
        &self.body
    }
}

pub(super) fn from_vec(body: Vec<u8>, permit: OwnedSemaphorePermit) -> Bytes {
    // Retain the slot as long as the transport owns any view of the buffer.
    Bytes::from_owner(BufferedResponse { body, _permit: permit })
}
