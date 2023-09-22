use std::num::NonZeroUsize;

/// Indexes into a single shared buffer
#[derive(Debug, Copy, Clone, Default)]
enum BufferHeads {
    #[default]
    // The Write buffer is empty, we're ready to read more data into the
    // buffer. No need to track a head in this case, we can always read to the
    // front of the buffer.
    ReadReady,

    // The Write buffer is full; there's no more room to read. This tracks the
    // point from which we should start writing.
    WriteReady(usize),

    // There's room for both writes and reads
    DuplexReady {
        /// The point at which we should start doing writes, out of the buffer
        write_head: usize,

        /// The point at which we should start doing reads, into of the buffer
        read_head: usize,
    },
}

impl BufferHeads {
    #[inline]
    pub fn advance_read(self, amount: NonZeroUsize, max: usize) -> Self {
        debug_assert!(amount.get() <= max);

        let (read_head, write_head) = match self {
            BufferHeads::ReadReady => (0, 0),
            BufferHeads::WriteReady(_) => panic!("advanced read of a full write buffer"),
            BufferHeads::DuplexReady {
                write_head,
                read_head,
            } => (read_head, write_head),
        };

        let read_head = (read_head + amount.get()) % max;

        match read_head == write_head {
            true => BufferHeads::WriteReady(write_head),
            false => BufferHeads::DuplexReady {
                write_head,
                read_head,
            },
        }
    }

    #[inline]
    pub fn advance_write(self, amount: NonZeroUsize, max: usize) -> Self {
        debug_assert!(amount.get() <= max);

        let (write_head, read_head) = match self {
            BufferHeads::ReadReady => panic!("advanced write of an empty write buffer"),
            BufferHeads::WriteReady(point) => (point, point),
            BufferHeads::DuplexReady {
                write_head,
                read_head,
            } => (write_head, read_head),
        };

        let write_head = (write_head + amount.get()) % max;

        match write_head == read_head {
            true => BufferHeads::ReadReady,
            false => BufferHeads::DuplexReady {
                write_head,
                read_head,
            },
        }
    }

    #[inline]
    #[must_use]
    pub fn read_ready(&self) -> bool {
        matches!(
            *self,
            BufferHeads::ReadReady | BufferHeads::DuplexReady { .. }
        )
    }

    #[inline]
    #[must_use]
    pub fn write_ready(&self) -> bool {
        matches!(
            *self,
            BufferHeads::WriteReady(..) | BufferHeads::DuplexReady { .. }
        )
    }
}

#[derive(Default, Clone, Copy)]
pub struct DuplexBuffer<B> {
    buffer: B,
    heads: BufferHeads,
}

/// A pair of pairs of buffers representing the current state
#[derive(Debug)]
pub struct Buffers<'a> {
    pub read: [&'a mut [u8]; 2],
    pub write: [&'a [u8]; 2],
}

/// Split a buffer into 3 units, at the given points: [..point1], [point1..point2], [point2..]
#[inline]
fn split_thrice(buffer: &mut [u8], point1: usize, point2: usize) -> [&mut [u8]; 3] {
    let (point1, point2) = if point1 <= point2 {
        (point1, point2)
    } else {
        (point2, point1)
    };

    let (head, b3) = buffer.split_at_mut(point2);
    let (b1, b2) = head.split_at_mut(point1);

    [b1, b2, b3]
}

impl<B: AsMut<[u8]>> DuplexBuffer<B> {
    pub fn new(buffer: B) -> Self {
        Self {
            buffer,
            heads: BufferHeads::default(),
        }
    }
}

impl<B: AsMut<[u8]>> DuplexBuffer<B> {
    pub fn get_buffers(&mut self) -> Buffers<'_> {
        let buffer = self.buffer.as_mut();

        match self.heads {
            BufferHeads::ReadReady => Buffers {
                read: [buffer, &mut []],
                write: [&[], &[]],
            },
            BufferHeads::WriteReady(point) => {
                let (head, tail) = buffer.split_at(point);
                Buffers {
                    read: [&mut [], &mut []],
                    write: [tail, head],
                }
            }
            BufferHeads::DuplexReady {
                write_head,
                read_head,
            } => {
                let [b1, b2, b3] = split_thrice(buffer, write_head, read_head);
                match write_head < read_head {
                    true => Buffers {
                        read: [b3, b1],
                        write: [b2, &[]],
                    },
                    false => Buffers {
                        read: [b2, &mut []],
                        write: [b3, b1],
                    },
                }
            }
        }
    }

    /// Returns true if we're able to read more data into the buffers
    #[inline]
    #[must_use]
    pub fn read_ready(&self) -> bool {
        self.heads.read_ready()
    }

    /// Returns true if we're able to write more data out of the buffers
    #[inline]
    #[must_use]
    pub fn write_ready(&self) -> bool {
        self.heads.write_ready()
    }

    #[inline]
    pub fn advance_read(&mut self, amount: NonZeroUsize) {
        self.heads = self.heads.advance_read(amount, self.buffer.as_mut().len())
    }

    #[inline]
    pub fn advance_write(&mut self, amount: NonZeroUsize) {
        self.heads = self.heads.advance_write(amount, self.buffer.as_mut().len())
    }
}

#[inline]
#[must_use]
pub const fn pair_len(&[b1, b2]: &[&[u8]; 2]) -> usize {
    b1.len() + b2.len()
}
