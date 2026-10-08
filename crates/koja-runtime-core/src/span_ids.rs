//! The span id generator. One per scheduler thread, seeded once from
//! the OS, so a sampled span costs no system call.
//!
//! The generator is xoshiro256**. Its output is not secret and a span
//! id only has to be unique within a trace, so a fast generator with a
//! long period is the right tool. The platform adapter supplies the
//! seed bytes. The core never touches the OS.

/// A xoshiro256** state. Never yields zero, because a zero span id
/// means "no span" in [`crate::Context`].
#[derive(Clone, Debug)]
pub struct SpanIds {
    state: [u64; 4],
}

impl SpanIds {
    /// Builds a generator from 32 seed bytes. An all-zero seed is
    /// replaced by a fixed non-zero state, since xoshiro is stuck at
    /// zero forever.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let mut state = [0u64; 4];
        for (word, chunk) in state.iter_mut().zip(seed.chunks_exact(8)) {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(chunk);
            *word = u64::from_le_bytes(bytes);
        }
        if state == [0; 4] {
            state = [
                0x9E37_79B9_7F4A_7C15,
                0xBF58_476D_1CE4_E5B9,
                0x94D0_49BB_1331_11EB,
                0x2545_F491_4F6C_DD1D,
            ];
        }
        Self { state }
    }

    /// The next non-zero id.
    pub fn next_id(&mut self) -> u64 {
        loop {
            let id = self.next_raw();
            if id != 0 {
                return id;
            }
        }
    }

    fn next_raw(&mut self) -> u64 {
        let s = &mut self.state;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::SpanIds;

    #[test]
    fn same_seed_same_sequence() {
        let seed = [7u8; 32];
        let mut a = SpanIds::from_seed(seed);
        let mut b = SpanIds::from_seed(seed);
        for _ in 0..16 {
            assert_eq!(a.next_id(), b.next_id());
        }
    }

    #[test]
    fn zero_seed_still_produces_ids() {
        let mut ids = SpanIds::from_seed([0u8; 32]);
        let first = ids.next_id();
        let second = ids.next_id();
        assert_ne!(first, 0);
        assert_ne!(first, second);
    }
}
