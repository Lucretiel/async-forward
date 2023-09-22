use async_forward::Forwarder;
use futures::{AsyncRead, AsyncWrite};

struct TestBuffer {
    data: Vec<u8>,
}
