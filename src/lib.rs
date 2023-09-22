mod buffer;

use std::{
    future::Future,
    io::{self, IoSlice, IoSliceMut},
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll},
};

use pin_project::pin_project;

use crate::buffer::{pair_len, DuplexBuffer};

#[pin_project]
pub struct Forwarder<R, W, B> {
    #[pin]
    reader: Option<R>,

    #[pin]
    writer: W,

    buffer: DuplexBuffer<B>,
}

impl<R: futures::AsyncRead, W: futures::AsyncWrite, B: AsMut<[u8]>> Forwarder<R, W, B> {
    pub fn new(reader: R, writer: W, buffer: B) -> Self {
        Self {
            reader: Some(reader),
            writer,
            buffer: DuplexBuffer::new(buffer),
        }
    }
}

#[derive(Debug)]
pub enum ForwarderError {
    Read(io::Error),
    Write(io::Error),
    WriteClosedEarly,
}

impl ForwarderError {
    pub fn into_io_error(self) -> io::Error {
        match self {
            Self::Read(err) => err,
            Self::Write(err) => err,
            Self::WriteClosedEarly => io::ErrorKind::WriteZero.into(),
        }
    }
}

impl<R: futures::AsyncRead, W: futures::AsyncWrite, B: AsMut<[u8]>> Future for Forwarder<R, W, B> {
    type Output = Result<(), ForwarderError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        // Basically: attempt to read once, then attempt to write once. If
        // a read or a write succeed but there's more relevant buffer available,
        // we signal the waker immediately. Smartly call wake if a read or write
        // returns Poll::Ready and we can immediately do more work (we don't
        // want an unbounded loop in here)

        // These are set to true after a successful read or write (or in some
        // other cases) to indicate that we should immediately wake the waker
        // because there's more work immediately possible
        let mut write_ready = false;
        let mut read_ready = false;

        if let Some(reader) = this.reader.as_mut().as_pin_mut() {
            let [b1, b2] = this.buffer.get_buffers().read;
            let read_buffer_len = pair_len(&[b1, b2]);

            // only perform a read if there's room
            if read_buffer_len > 0 {
                match reader.poll_read_vectored(cx, &mut [IoSliceMut::new(b1), IoSliceMut::new(b2)])
                {
                    // We're waiting for more read data. This registered the
                    // waker, so we'll get polled when we can do more reading.
                    Poll::Pending => {}
                    Poll::Ready(Err(err)) if err.kind() == io::ErrorKind::WouldBlock => {}

                    Poll::Ready(Ok(n)) => match NonZeroUsize::new(n) {
                        // Nothing else available to read. Clear the reader and
                        // proceed to write whatever's left in the buffer
                        None => this.reader.set(None),

                        // Read some data. Advance the buffer, and additionally
                        // fire a signal that we want to be polled immediately to
                        // read more data if there's space available.
                        Some(n) => {
                            this.buffer.advance_read(n);
                            read_ready = true;
                        }
                    },

                    // If we were interrupted, we can retry the read. We don't
                    // want to potentially block forever, though, so signal
                    // the executor that we want to be polled again.
                    Poll::Ready(Err(err)) if err.kind() == io::ErrorKind::Interrupted => {
                        read_ready = true;
                    }

                    // There was a real error; return it.
                    Poll::Ready(Err(err)) => return Poll::Ready(Err(ForwarderError::Read(err))),
                }
            }
        }

        // The read might have advanced the buffer, so get a fresh set of write
        // buffers
        let [b1, b2] = this.buffer.get_buffers().write;
        let write_buffer_len = pair_len(&[b1, b2]);

        // Only perform a write if there's data to be written
        if write_buffer_len > 0 {
            match this
                .writer
                .poll_write_vectored(cx, &[IoSlice::new(b1), IoSlice::new(b2)])
            {
                // We're waiting for more availability to write. Nothing else to
                // be done at this point.
                Poll::Pending => {}
                Poll::Ready(Err(err)) if err.kind() == io::ErrorKind::WouldBlock => {}

                Poll::Ready(Ok(n)) => match NonZeroUsize::new(n) {
                    // The writer is closed before we could forward everything.
                    // This is a problem.
                    None => return Poll::Ready(Err(ForwarderError::WriteClosedEarly)),

                    // We wrote some data. Advance the buffer, and additionally
                    // fire a signal that we want to be polled immediately to
                    // write more data if there's data available.
                    Some(n) => {
                        this.buffer.advance_write(n);
                        write_ready = true
                    }
                },

                Poll::Ready(Err(err)) if err.kind() == io::ErrorKind::Interrupted => {
                    write_ready = true
                }

                Poll::Ready(Err(err)) => return Poll::Ready(Err(ForwarderError::Write(err))),
            }
        }

        // We've made at most one read and one write. If, at this point, the
        // reader is done and the write buffer is empty, we're done.
        if this.reader.is_none() && !this.buffer.write_ready() {
            return Poll::Ready(Ok(()));
        }

        if (write_ready && this.buffer.write_ready()) || (read_ready && this.buffer.read_ready()) {
            cx.waker().wake_by_ref();
        }

        Poll::Pending
    }
}
