//! MCCP2: the server's zlib stream, which starts right after
//! `IAC SB COMPRESS2 IAC SE` and carries telnet until it ends.

use super::{Bytes, CompressionError};
use flate2::{Decompress, FlushDecompress, Status};

/// Output is produced in blocks of this size: one block holds a typical
/// server write, and text slices still held pin no more than that.
const BLOCK: usize = 16 * 1024;

#[derive(Debug)]
pub(super) struct Inflater {
    stream: Decompress,
}

/// The result of one decompression step.
pub(super) struct Inflated {
    /// How many input bytes the step used.
    pub consumed: usize,
    pub output: Bytes,
    /// The output block filled up, so more may be waiting.
    pub more: bool,
    /// The compressed stream ended; any input after `consumed` is plain.
    pub ended: bool,
}

impl Inflater {
    pub fn new() -> Self {
        Inflater {
            stream: Decompress::new(true),
        }
    }

    pub fn inflate(&mut self, input: &[u8]) -> Result<Inflated, CompressionError> {
        let mut output = Vec::with_capacity(BLOCK);
        let consumed_before = self.stream.total_in();
        let status = self
            .stream
            .decompress_vec(input, &mut output, FlushDecompress::None)
            .map_err(|error| CompressionError(error.to_string()))?;
        let consumed = usize::try_from(self.stream.total_in() - consumed_before)
            .expect("a step consumes at most its input");
        Ok(Inflated {
            consumed,
            more: output.len() == output.capacity(),
            ended: status == Status::StreamEnd,
            output: Bytes::from(output),
        })
    }
}
