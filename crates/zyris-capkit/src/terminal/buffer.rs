use std::collections::VecDeque;

/// An absolute-offset ring buffer holding a session's output.
///
/// The cursor is not kept inside it — a session's `read` cursor and an `open_stream` subscriber's
/// cursor need to be independent of each other. The caller holds the cursor and lends it to
/// `read_at`.
pub(crate) struct OutputBuffer {
    cap: usize,
    ring: VecDeque<u8>,
    /// Total bytes the PTY has produced so far. Does not shrink when bytes are pushed out of the ring.
    total_written: u64,
}

impl OutputBuffer {
    pub(crate) fn new(cap: usize) -> Self {
        OutputBuffer { cap, ring: VecDeque::new(), total_written: 0 }
    }

    pub(crate) fn total_written(&self) -> u64 {
        self.total_written
    }

    /// The absolute offset of the oldest byte still left in the ring.
    fn ring_start(&self) -> u64 {
        self.total_written - self.ring.len() as u64
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.total_written += bytes.len() as u64;
        // If a single push is larger than the ring, only the trailing `cap` bytes matter.
        let tail = if bytes.len() > self.cap { &bytes[bytes.len() - self.cap..] } else { bytes };
        self.ring.extend(tail);
        let overflow = self.ring.len().saturating_sub(self.cap);
        if overflow > 0 {
            self.ring.drain(..overflow);
        }
    }

    /// Reads up to `max` bytes starting at `cursor` and advances the cursor.
    ///
    /// The returned `u64` is the number of bytes **lost for good** — however much the cursor got
    /// pushed out past the ring's edge. That means it cannot be recovered even by calling again,
    /// which is a different fact from `more` ("there's still more left").
    pub(crate) fn read_at(&self, cursor: &mut u64, max: usize) -> (Vec<u8>, u64) {
        let start = self.ring_start();
        let dropped = start.saturating_sub(*cursor);
        if dropped > 0 {
            *cursor = start;
        }
        let from = (*cursor - start) as usize;
        let n = (self.ring.len() - from).min(max);
        let bytes: Vec<u8> = self.ring.iter().skip(from).take(n).copied().collect();
        *cursor += n as u64;
        (bytes, dropped)
    }
}

/// Length with a truncated multibyte sequence excluded from `b`. That tail carries over to the
/// next call.
///
/// A chunk boundary can fall in the middle of a UTF-8 character. Emitting a replacement character
/// there loses that character for good — it loses the chance to be joined with the bytes that
/// would have followed.
pub(crate) fn trim_incomplete_tail(b: &[u8]) -> usize {
    let max_back = 3.min(b.len());
    for back in 1..=max_back {
        let i = b.len() - back;
        let c = b[i];
        if c < 0x80 {
            return b.len(); // ASCII — this is the boundary
        }
        if c >= 0xC0 {
            let need = if c >= 0xF0 {
                4
            } else if c >= 0xE0 {
                3
            } else {
                2
            };
            return if back < need { i } else { b.len() };
        }
        // continuation byte (0x80..=0xBF) — keep stepping back to find the lead byte
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_everything_written() {
        let mut b = OutputBuffer::new(64);
        b.push(b"hello");
        let mut c = 0u64;
        let (bytes, dropped) = b.read_at(&mut c, 1024);
        assert_eq!(bytes, b"hello");
        assert_eq!(dropped, 0);
        assert_eq!(c, 5);
        // The second read comes back empty — the cursor is already at the end.
        let (bytes, dropped) = b.read_at(&mut c, 1024);
        assert!(bytes.is_empty());
        assert_eq!(dropped, 0);
    }

    #[test]
    fn max_bounds_one_read_and_leaves_the_rest() {
        let mut b = OutputBuffer::new(64);
        b.push(b"abcdefghij");
        let mut c = 0u64;
        assert_eq!(b.read_at(&mut c, 4).0, b"abcd");
        assert_eq!(c, 4);
        assert_eq!(b.read_at(&mut c, 4).0, b"efgh");
        assert_eq!(b.read_at(&mut c, 4).0, b"ij");
    }

    /// If the ring overflows, the number of bytes lost must come back exactly as `dropped`.
    /// If this value is wrong, the agent has no way to tell whether calling again would recover it.
    #[test]
    fn overflow_reports_exactly_how_many_bytes_were_lost() {
        let mut b = OutputBuffer::new(10);
        b.push(b"0123456789"); // the ring is now full
        b.push(b"abcde"); // pushes the first 5 bytes out
        assert_eq!(b.total_written(), 15);

        let mut c = 0u64;
        let (bytes, dropped) = b.read_at(&mut c, 1024);
        assert_eq!(dropped, 5);
        assert_eq!(bytes, b"56789abcde");
        assert_eq!(c, 15);
    }

    /// If the cursor is already inside the ring, nothing has been lost.
    #[test]
    fn a_cursor_still_inside_the_ring_loses_nothing() {
        let mut b = OutputBuffer::new(10);
        b.push(b"0123456789");
        let mut c = 0u64;
        b.read_at(&mut c, 1024); // c == 10
        b.push(b"abcde");
        let (bytes, dropped) = b.read_at(&mut c, 1024);
        assert_eq!(dropped, 0);
        assert_eq!(bytes, b"abcde");
    }

    /// Even when a single push is larger than the ring, only the last `cap` bytes should survive
    /// and the accounting should still add up.
    #[test]
    fn a_single_push_larger_than_the_ring_keeps_only_the_tail() {
        let mut b = OutputBuffer::new(4);
        b.push(b"abcdefgh");
        assert_eq!(b.total_written(), 8);
        let mut c = 0u64;
        let (bytes, dropped) = b.read_at(&mut c, 1024);
        assert_eq!(bytes, b"efgh");
        assert_eq!(dropped, 4);
    }

    #[test]
    fn a_complete_sequence_is_not_held_back() {
        assert_eq!(trim_incomplete_tail("가".as_bytes()), 3);
        assert_eq!(trim_incomplete_tail(b"abc"), 3);
        assert_eq!(trim_incomplete_tail(b""), 0);
    }

    /// A truncated multibyte character carries over to the next call — burning it as a
    /// replacement character loses it for good.
    #[test]
    fn an_incomplete_tail_is_held_back() {
        let ga = "가".as_bytes(); // 3 bytes
        assert_eq!(trim_incomplete_tail(&ga[..1]), 0);
        assert_eq!(trim_incomplete_tail(&ga[..2]), 0);
        let mixed = [b"ab", &ga[..2][..]].concat();
        assert_eq!(trim_incomplete_tail(&mixed), 2);
        let emoji = "🎯".as_bytes(); // 4 bytes
        assert_eq!(trim_incomplete_tail(&emoji[..3]), 0);
        assert_eq!(trim_incomplete_tail(emoji), 4);
    }
}
