/// Minimal xorshift32 PRNG, seeded from the system tick counter at startup.
///
/// Only used to jitter retry backoff delays and mesh timing, not for anything
/// security-sensitive, so it deliberately avoids depending on a hardware RNG
/// peripheral (not present on all boards, e.g. STM32F103).
pub struct Prng(u32);

impl Prng {
    pub fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0xA5A5_A5A5 } else { seed })
    }

    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}
