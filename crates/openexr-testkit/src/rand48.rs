// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of Imath's `Rand48` (src/Imath/ImathRandom.h:59-85, src/Imath/ImathRandom.cpp:1-125
//! @ Imath v3.2.3), the generator upstream's tests fill images with: Imath's own `nrand48`,
//! the 48-bit linear congruential sequence of `drand48` and company.

/// Port of `Imath::Rand48` (src/Imath/ImathRandom.h:62-85 @ Imath v3.2.3).
#[derive(Debug, Clone)]
pub struct Rand48 {
    state: [u16; 3],
}

impl Rand48 {
    /// Port of `Rand48::Rand48` and `Rand48::init` (src/Imath/ImathRandom.h:162-175
    /// @ Imath v3.2.3). `unsigned long` is 32 bits on Windows and 64 on Linux; the state keeps
    /// only the low 32 bits of the scrambled seed, which are the same either way.
    pub fn new(seed: u64) -> Rand48 {
        let seed = seed.wrapping_mul(0xa5a573a5) ^ 0x5a5a5a5a;
        Rand48 {
            state: [
                (seed & 0xFFFF) as u16,
                ((seed >> 16) & 0xFFFF) as u16,
                (seed & 0xFFFF) as u16,
            ],
        }
    }

    /// Port of `Rand48::nexti` (src/Imath/ImathRandom.h:183-187 @ Imath v3.2.3) and
    /// `nrand48` (src/Imath/ImathRandom.cpp:115-125 @ Imath v3.2.3): the next value, in
    /// `[0, 0x7fffffff]`.
    pub fn nexti(&mut self) -> i64 {
        rand48_next(&mut self.state);
        (i64::from(self.state[2]) << 15) | (i64::from(self.state[1]) >> 1)
    }
}

/// Port of `rand48Next` (src/Imath/ImathRandom.cpp:26-69 @ Imath v3.2.3): `x = a * x + c`
/// modulo 2^48 on the state's three 16-bit words.
fn rand48_next(state: &mut [u16; 3]) {
    const A: u64 = 0x5deece66d;
    const C: u64 = 0xb;
    let x = (u64::from(state[2]) << 32) | (u64::from(state[1]) << 16) | u64::from(state[0]);
    let x = A.wrapping_mul(x).wrapping_add(C);
    state[2] = (x >> 32) as u16;
    state[1] = (x >> 16) as u16;
    state[0] = x as u16;
}
