use std::collections::VecDeque;

/// 세션이 낸 출력을 담는 절대 오프셋 링버퍼.
///
/// 커서를 안에 두지 않는다 — 세션의 `read` 커서와 `open_stream` 구독자의 커서가
/// 서로 독립이어야 하기 때문이다. 커서는 호출자가 들고 `read_at`에 빌려준다.
pub(crate) struct OutputBuffer {
    cap: usize,
    ring: VecDeque<u8>,
    /// 지금까지 PTY가 낸 총 바이트. 링에서 밀려나도 줄지 않는다.
    total_written: u64,
}

impl OutputBuffer {
    pub(crate) fn new(cap: usize) -> Self {
        OutputBuffer { cap, ring: VecDeque::new(), total_written: 0 }
    }

    pub(crate) fn total_written(&self) -> u64 {
        self.total_written
    }

    /// 링에 남은 가장 오래된 바이트의 절대 오프셋.
    fn ring_start(&self) -> u64 {
        self.total_written - self.ring.len() as u64
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.total_written += bytes.len() as u64;
        // 한 번의 push가 링보다 크면 뒤쪽 `cap` 바이트만 의미가 있다.
        let tail = if bytes.len() > self.cap { &bytes[bytes.len() - self.cap..] } else { bytes };
        self.ring.extend(tail);
        let overflow = self.ring.len().saturating_sub(self.cap);
        if overflow > 0 {
            self.ring.drain(..overflow);
        }
    }

    /// `cursor`부터 최대 `max` 바이트를 읽고 커서를 전진시킨다.
    ///
    /// 반환하는 `u64`는 **영원히 잃은** 바이트 수다 — 커서가 링 밖으로 밀려난 만큼.
    /// 다시 불러도 받을 수 없다는 뜻이라, "아직 남았다"는 뜻의 `more`와 다른 사실이다.
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

/// `b`에서 잘린 멀티바이트 시퀀스를 뺀 길이. 그 꼬리는 다음 호출로 넘어간다.
///
/// 청크 경계는 UTF-8 문자 한가운데 떨어질 수 있다. 거기서 대체 문자를 찍으면 그 글자는
/// 영영 못 살아난다 — 뒤따라 올 바이트와 합칠 기회가 사라지기 때문이다.
pub(crate) fn trim_incomplete_tail(b: &[u8]) -> usize {
    let max_back = 3.min(b.len());
    for back in 1..=max_back {
        let i = b.len() - back;
        let c = b[i];
        if c < 0x80 {
            return b.len(); // ASCII — 여기가 경계다
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
        // 계속 바이트(0x80..=0xBF) — 선두를 찾아 더 되짚는다
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
        // 두 번째 읽기는 비어 있다 — 커서가 이미 끝이다.
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

    /// 링이 넘치면 잃은 바이트 수가 정확히 `dropped`로 나와야 한다.
    /// 이 값이 틀리면 에이전트가 "다시 부르면 받을 수 있는가"를 판단할 수 없다.
    #[test]
    fn overflow_reports_exactly_how_many_bytes_were_lost() {
        let mut b = OutputBuffer::new(10);
        b.push(b"0123456789"); // 링이 꽉 찼다
        b.push(b"abcde"); // 앞의 5바이트가 밀려났다
        assert_eq!(b.total_written(), 15);

        let mut c = 0u64;
        let (bytes, dropped) = b.read_at(&mut c, 1024);
        assert_eq!(dropped, 5);
        assert_eq!(bytes, b"56789abcde");
        assert_eq!(c, 15);
    }

    /// 커서가 이미 링 안에 있으면 아무것도 잃지 않았다.
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

    /// 한 번의 push가 링보다 커도 마지막 `cap` 바이트만 남고 회계는 맞아야 한다.
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

    /// 잘린 멀티바이트 문자는 다음 호출로 넘긴다 — 대체 문자로 태우면 영영 못 살린다.
    #[test]
    fn an_incomplete_tail_is_held_back() {
        let ga = "가".as_bytes(); // 3바이트
        assert_eq!(trim_incomplete_tail(&ga[..1]), 0);
        assert_eq!(trim_incomplete_tail(&ga[..2]), 0);
        let mixed = [b"ab", &ga[..2][..]].concat();
        assert_eq!(trim_incomplete_tail(&mixed), 2);
        let emoji = "🎯".as_bytes(); // 4바이트
        assert_eq!(trim_incomplete_tail(&emoji[..3]), 0);
        assert_eq!(trim_incomplete_tail(emoji), 4);
    }
}
