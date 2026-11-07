// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! C `printf`, byte for byte as the C runtime of each reference platform formats it (PLAN.md
//! §3, "How we get bit-exact results").
//!
//! Core builds its error and debug text with `vsnprintf` (`dispatch_print_error`,
//! `internal_structs.c:104-142` @ v3.5.3) and `printf` (`debug.c`). This module provides the
//! conversions Core's format strings use, nothing more: `%d %i %u %x %X %o %c %s %p`, `%e %f
//! %g` and their upper-case forms, with flags, width, precision and length modifiers; the
//! argument's own Rust type decides its width, as the C promotions do. `%n` and `*` widths
//! are not used by Core and not provided.
//!
//! The float conversions expand the binary value exactly and round half to even, which is what
//! UCRT (since VS 2019 16.2) and glibc both do; they differ only in how they spell NaN, and in
//! `%p`. The float code is OpenColorIO-rs's (`crates/ocio-ops/src/cfmt.rs`), whose sweep against
//! both C runtimes established it. `crates/openexr-testkit` checks this module against the
//! platform's own `snprintf`.
//!
//! Text is bytes, as a C string's: a `%s` argument that is not UTF-8 passes through unchanged.

use std::fmt::Write as _;

// Only the two reference platforms (PLAN.md D11) have a known C runtime.
#[cfg(not(any(
    all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"),
    all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"),
)))]
compile_error!(
    "cfmt knows the C runtimes of x86_64 Windows (MSVC, UCRT) and x86_64 Linux (glibc) only"
);

/// The C runtime whose conventions to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crt {
    /// The Universal CRT (Windows).
    Ucrt,
    /// GNU libc (Linux).
    Glibc,
}

impl Crt {
    /// The C runtime of the platform being compiled for: the UCRT on x86_64 Windows (MSVC).
    #[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"))]
    pub const NATIVE: Crt = Crt::Ucrt;

    /// The C runtime of the platform being compiled for: glibc on x86_64 Linux.
    #[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
    pub const NATIVE: Crt = Crt::Glibc;
}

/// One `printf` argument, with the width C passes it at after the default promotions.
#[derive(Debug, Clone, Copy)]
pub enum Arg<'a> {
    /// `int` (also `char`, `short`, `int8_t`, `int16_t` after promotion).
    I32(i32),
    /// `unsigned int` (also `uint8_t`, `uint16_t` after promotion).
    U32(u32),
    /// `int64_t`, `long long`.
    I64(i64),
    /// `uint64_t`, `unsigned long long`.
    U64(u64),
    /// `size_t`.
    Usize(usize),
    /// `double` (also `float` after promotion).
    F64(f64),
    /// A non-NULL `const char*`: the bytes before its NUL.
    Str(&'a [u8]),
    /// A NULL `const char*`.
    NullStr,
    /// A pointer, for `%p`.
    Ptr(usize),
}

macro_rules! arg_from {
    ($($t:ty => $v:ident),* $(,)?) => {
        $(impl From<$t> for Arg<'_> {
            fn from(v: $t) -> Self {
                Arg::$v(v.into())
            }
        })*
    };
}
arg_from!(i32 => I32, i16 => I32, i8 => I32, u32 => U32, u16 => U32, u8 => U32,
          i64 => I64, u64 => U64, usize => Usize, f64 => F64, f32 => F64);

impl<'a> From<&'a str> for Arg<'a> {
    fn from(v: &'a str) -> Self {
        Arg::Str(v.as_bytes())
    }
}

impl<'a> From<&'a [u8]> for Arg<'a> {
    fn from(v: &'a [u8]) -> Self {
        Arg::Str(v)
    }
}

impl<'a> From<&'a String> for Arg<'a> {
    fn from(v: &'a String) -> Self {
        Arg::Str(v.as_bytes())
    }
}

/// `snprintf` into a byte string: formats `args` by `format` as `crt` does.
///
/// Panics if a conversion has no argument or the arguments outlast the conversions: a format
/// and its arguments are fixed at each call site, so that is a bug in the port, as it would be
/// undefined behaviour in C.
pub fn sprintf(crt: Crt, format: &str, args: &[Arg<'_>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(format.len() + 16);
    let fmt = format.as_bytes();
    let mut args = args.iter();
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        if fmt.get(i) == Some(&b'%') {
            out.push(b'%');
            i += 1;
            continue;
        }
        let (spec, next) =
            Spec::parse(fmt, i).unwrap_or_else(|| panic!("unsupported format {format:?}"));
        i = next;
        let arg = args
            .next()
            .unwrap_or_else(|| panic!("missing argument for {format:?}"));
        spec.write(crt, arg, &mut out);
    }
    assert!(args.next().is_none(), "too many arguments for {format:?}");
    out
}

/// `sprintf!(crt, "fmt", a, b, ...)`: [`sprintf`] with each argument converted by `Arg::from`.
#[macro_export]
#[doc(hidden)]
macro_rules! sprintf {
    ($crt:expr, $fmt:expr $(, $arg:expr)* $(,)?) => {
        $crate::cfmt::sprintf($crt, $fmt, &[$($crate::cfmt::Arg::from($arg)),*])
    };
}

/// One conversion: `%[flags][width][.precision][length]conv`.
#[derive(Debug, Clone, Copy)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
    conv: u8,
}

impl Spec {
    /// Parses the conversion starting after its `%` at `fmt[i]`; returns it and the index after it.
    fn parse(fmt: &[u8], mut i: usize) -> Option<(Spec, usize)> {
        let mut s = Spec {
            left: false,
            plus: false,
            space: false,
            alt: false,
            zero: false,
            width: 0,
            precision: None,
            conv: 0,
        };
        while let Some(&c) = fmt.get(i) {
            match c {
                b'-' => s.left = true,
                b'+' => s.plus = true,
                b' ' => s.space = true,
                b'#' => s.alt = true,
                b'0' => s.zero = true,
                _ => break,
            }
            i += 1;
        }
        let number = |i: &mut usize| -> usize {
            let mut n = 0usize;
            while let Some(d) = fmt.get(*i).filter(|c| c.is_ascii_digit()) {
                n = n * 10 + usize::from(d - b'0');
                *i += 1;
            }
            n
        };
        s.width = number(&mut i);
        if fmt.get(i) == Some(&b'.') {
            i += 1;
            s.precision = Some(number(&mut i));
        }
        // Length modifiers: the argument's type carries the width, so they are only skipped.
        // `I64` is MSVC's spelling inside its PRId64/PRIu64.
        if fmt[i..].starts_with(b"I64") {
            i += 3;
        }
        while let Some(b'h' | b'l' | b'L' | b'q' | b'j' | b'z' | b't') = fmt.get(i) {
            i += 1;
        }
        let conv = *fmt.get(i)?;
        if !b"diuxXocspeEfFgG".contains(&conv) {
            return None;
        }
        s.conv = conv;
        Some((s, i + 1))
    }

    fn write(&self, crt: Crt, arg: &Arg<'_>, out: &mut Vec<u8>) {
        match self.conv {
            b'd' | b'i' => {
                let v: i128 = match *arg {
                    Arg::I32(v) => v.into(),
                    Arg::U32(v) => (v as i32).into(),
                    Arg::I64(v) => v.into(),
                    Arg::U64(v) => (v as i64).into(),
                    Arg::Usize(v) => (v as i64).into(),
                    _ => panic!("%d of {arg:?}"),
                };
                let sign = if v < 0 {
                    "-"
                } else if self.plus {
                    "+"
                } else if self.space {
                    " "
                } else {
                    ""
                };
                self.integer(sign, "", v.unsigned_abs().to_string(), out);
            }
            b'u' | b'x' | b'X' | b'o' => {
                let v: u64 = match *arg {
                    Arg::I32(v) => u64::from(v as u32),
                    Arg::U32(v) => v.into(),
                    Arg::I64(v) => v as u64,
                    Arg::U64(v) => v,
                    Arg::Usize(v) => v as u64,
                    _ => panic!("%{} of {arg:?}", self.conv as char),
                };
                let (digits, prefix) = match self.conv {
                    b'u' => (v.to_string(), ""),
                    b'x' => (format!("{v:x}"), if self.alt && v != 0 { "0x" } else { "" }),
                    b'X' => (format!("{v:X}"), if self.alt && v != 0 { "0X" } else { "" }),
                    _ => {
                        let d = format!("{v:o}");
                        if self.alt && !d.starts_with('0') {
                            (format!("0{d}"), "")
                        } else {
                            (d, "")
                        }
                    }
                };
                self.integer("", prefix, digits, out);
            }
            b'c' => {
                let v = match *arg {
                    Arg::I32(v) => v as u8,
                    Arg::U32(v) => v as u8,
                    _ => panic!("%c of {arg:?}"),
                };
                self.pad(&[], &[v], false, out);
            }
            b's' => {
                let bytes: &[u8] = match *arg {
                    Arg::Str(s) => s,
                    // Both runtimes print "(null)" for a NULL string (glibc only when the
                    // precision allows all of it; Core never passes a precision with NULL).
                    Arg::NullStr => b"(null)",
                    _ => panic!("%s of {arg:?}"),
                };
                let n = self.precision.map_or(bytes.len(), |p| p.min(bytes.len()));
                self.pad(&[], &bytes[..n], false, out);
            }
            b'p' => {
                let Arg::Ptr(v) = *arg else {
                    panic!("%p of {arg:?}");
                };
                let text = match crt {
                    // UCRT: 16 upper-case hex digits, zero-filled.
                    Crt::Ucrt => format!("{v:016X}"),
                    Crt::Glibc if v == 0 => "(nil)".to_string(),
                    Crt::Glibc => format!("0x{v:x}"),
                };
                self.pad(&[], text.as_bytes(), false, out);
            }
            _ => {
                let Arg::F64(v) = *arg else {
                    panic!("%{} of {arg:?}", self.conv as char);
                };
                self.float(crt, v, out);
            }
        }
    }

    /// Pads `prefix` + `body` to the width: spaces on the left, or on the right with `-`, or
    /// zeros between them when `zeros`.
    fn pad(&self, prefix: &[u8], body: &[u8], zeros: bool, out: &mut Vec<u8>) {
        let len = prefix.len() + body.len();
        let fill = self.width.saturating_sub(len);
        if self.left {
            out.extend_from_slice(prefix);
            out.extend_from_slice(body);
            out.extend(std::iter::repeat_n(b' ', fill));
        } else if zeros {
            out.extend_from_slice(prefix);
            out.extend(std::iter::repeat_n(b'0', fill));
            out.extend_from_slice(body);
        } else {
            out.extend(std::iter::repeat_n(b' ', fill));
            out.extend_from_slice(prefix);
            out.extend_from_slice(body);
        }
    }

    /// An integer conversion: the precision is a minimum digit count (with precision 0, the
    /// value 0 prints no digits), and `0` pads only without a precision and without `-`.
    fn integer(&self, sign: &str, alt_prefix: &str, digits: String, out: &mut Vec<u8>) {
        let mut body = match self.precision {
            Some(0) if digits == "0" => String::new(),
            Some(p) if digits.len() < p => "0".repeat(p - digits.len()) + &digits,
            _ => digits,
        };
        if self.conv == b'o' && self.alt && body.is_empty() {
            body.push('0');
        }
        let prefix = format!("{sign}{alt_prefix}");
        let zeros = self.zero && !self.left && self.precision.is_none();
        self.pad(prefix.as_bytes(), body.as_bytes(), zeros, out);
    }

    fn float(&self, crt: Crt, value: f64, out: &mut Vec<u8>) {
        let upper = self.conv.is_ascii_uppercase();
        let bits = value.to_bits();
        let negative = bits >> 63 == 1;
        let finite = value.is_finite();
        let is_g = matches!(self.conv, b'g' | b'G');
        let mut precision = self.precision.unwrap_or(6);
        // A precision of 0 means 1 for %g (C17 7.21.6.1p8).
        if is_g && precision == 0 {
            precision = 1;
        }
        assert!(
            !self.alt,
            "the # flag is not provided for floating-point conversions"
        );
        let mut body = if !finite {
            special_body(crt, bits, upper).to_string()
        } else {
            let exact = (value != 0.0).then(|| exact_decimal(value));
            match self.conv {
                b'e' | b'E' => body_e(exact.as_ref(), precision, upper),
                b'f' | b'F' => body_f(exact.as_ref(), precision),
                _ => body_g(exact.as_ref(), precision, upper),
            }
        };
        if is_g {
            remove_g_trailing_zeros(&mut body);
        }
        let sign = if negative {
            "-"
        } else if self.plus {
            "+"
        } else if self.space {
            " "
        } else {
            ""
        };
        // Both libraries pad infinities and NaNs with spaces, even under '0'.
        let zeros = self.zero && !self.left && finite;
        self.pad(sign.as_bytes(), body.as_bytes(), zeros, out);
    }
}

// ---------------------------------------------------------------------------------------
// Exact decimal expansion (from OpenColorIO-rs, crates/ocio-ops/src/cfmt.rs)
// ---------------------------------------------------------------------------------------

/// An unsigned integer as little-endian base-2^32 limbs, without leading zero limbs.
#[derive(Debug, Clone)]
struct Big(Vec<u32>);

impl Big {
    fn from_u64(v: u64) -> Big {
        let mut b = Big(vec![v as u32, (v >> 32) as u32]);
        b.trim();
        b
    }

    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for limb in &mut self.0 {
            let v = u64::from(*limb) * u64::from(m) + carry;
            *limb = v as u32;
            carry = v >> 32;
        }
        if carry != 0 {
            self.0.push(carry as u32);
        }
    }

    fn mul_pow5(&mut self, mut k: u32) {
        const POW5_13: u32 = 1_220_703_125; // 5^13, the largest power of 5 below 2^32
        while k >= 13 {
            self.mul_small(POW5_13);
            k -= 13;
        }
        self.mul_small(5u32.pow(k));
    }

    fn shl(&mut self, bits: u32) {
        let limbs = (bits / 32) as usize;
        let rest = bits % 32;
        if rest != 0 {
            let mut carry = 0u32;
            for limb in &mut self.0 {
                let v = *limb;
                *limb = (v << rest) | carry;
                carry = v >> (32 - rest);
            }
            if carry != 0 {
                self.0.push(carry);
            }
        }
        if limbs != 0 {
            self.0.splice(0..0, std::iter::repeat_n(0, limbs));
        }
    }

    /// Divides in place and returns the remainder.
    fn divmod_small(&mut self, d: u32) -> u32 {
        let mut rem = 0u64;
        for limb in self.0.iter_mut().rev() {
            let v = (rem << 32) | u64::from(*limb);
            *limb = (v / u64::from(d)) as u32;
            rem = v % u64::from(d);
        }
        self.trim();
        rem as u32
    }

    /// Decimal digits (values 0-9), most significant first. Empty for zero.
    fn into_decimal(mut self) -> Vec<u8> {
        const CHUNK: u32 = 1_000_000_000;
        let mut chunks = Vec::new();
        while !self.0.is_empty() {
            chunks.push(self.divmod_small(CHUNK));
        }
        let mut digits = Vec::with_capacity(chunks.len() * 9);
        for (i, chunk) in chunks.iter().rev().enumerate() {
            let mut buf = [0u8; 9];
            let mut c = *chunk;
            for slot in buf.iter_mut().rev() {
                *slot = (c % 10) as u8;
                c /= 10;
            }
            let skip = if i == 0 {
                buf.iter().position(|&d| d != 0).unwrap_or(9)
            } else {
                0
            };
            digits.extend_from_slice(&buf[skip..]);
        }
        digits
    }
}

/// The exact decimal value of a finite, nonzero magnitude: `0.d[0] d[1] ... * 10^exp10`, with
/// `d[0] != 0` and no trailing zero digits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Exact {
    digits: Vec<u8>,
    exp10: i32,
}

/// Expands `|value|` (finite, nonzero) exactly.
fn exact_decimal(value: f64) -> Exact {
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (m, e) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased - 1075)
    };
    let (mut m, mut e) = (m, e);
    let zeros = m.trailing_zeros();
    m >>= zeros;
    e += zeros as i32;

    let mut n = Big::from_u64(m);
    let scale10 = if e >= 0 {
        n.shl(e as u32);
        0
    } else {
        // m * 2^e = m * 5^-e / 10^-e
        n.mul_pow5((-e) as u32);
        e
    };
    let mut digits = n.into_decimal();
    let exp10 = digits.len() as i32 + scale10;
    while digits.last() == Some(&0) {
        digits.pop();
    }
    Exact { digits, exp10 }
}

/// Rounds `exact` to a multiple of `10^unit_exp10`, halves to even, and returns the multiple
/// as decimal digits (most significant first; empty for zero).
fn round_to_unit(exact: &Exact, unit_exp10: i32) -> Vec<u8> {
    let keep = exact.exp10 - unit_exp10;
    let d = &exact.digits;
    if keep < 0 {
        return Vec::new();
    }
    let keep = keep as usize;
    if keep >= d.len() {
        let mut q = d.clone();
        q.resize(keep, 0);
        return q;
    }
    let mut q = d[..keep].to_vec();
    let next = d[keep];
    let sticky = d[keep + 1..].iter().any(|&x| x != 0);
    let odd = q.last().is_some_and(|&x| x % 2 == 1);
    if next > 5 || (next == 5 && (sticky || odd)) {
        let mut i = q.len();
        loop {
            if i == 0 {
                q.insert(0, 1);
                break;
            }
            i -= 1;
            if q[i] == 9 {
                q[i] = 0;
            } else {
                q[i] += 1;
                break;
            }
        }
    }
    let first = q.iter().position(|&x| x != 0).unwrap_or(q.len());
    q.drain(..first);
    q
}

fn push_exponent(out: &mut String, upper: bool, x: i32) {
    out.push(if upper { 'E' } else { 'e' });
    out.push(if x < 0 { '-' } else { '+' });
    let _ = write!(out, "{:02}", x.unsigned_abs());
}

fn push_digits(out: &mut String, digits: &[u8]) {
    out.extend(digits.iter().map(|&d| char::from(b'0' + d)));
}

/// `%e` of a finite magnitude: the body without sign.
fn body_e(exact: Option<&Exact>, precision: usize, upper: bool) -> String {
    let (digits, x) = match exact {
        None => (vec![0; precision + 1], 0),
        Some(ex) => {
            let unit = ex.exp10 - 1 - precision as i32;
            let mut q = round_to_unit(ex, unit);
            let mut x = ex.exp10 - 1;
            if q.len() > precision + 1 {
                q.pop();
                x += 1;
            }
            (q, x)
        }
    };
    let mut out = String::new();
    push_digits(&mut out, &digits[..1]);
    if precision > 0 {
        out.push('.');
    }
    push_digits(&mut out, &digits[1..]);
    push_exponent(&mut out, upper, x);
    out
}

/// `%f` of a finite magnitude: the body without sign.
fn body_f(exact: Option<&Exact>, precision: usize) -> String {
    let q = match exact {
        None => Vec::new(),
        Some(ex) => round_to_unit(ex, -(precision as i32)),
    };
    let mut out = String::new();
    if q.len() > precision {
        push_digits(&mut out, &q[..q.len() - precision]);
    } else {
        out.push('0');
    }
    if precision > 0 {
        out.push('.');
    }
    let frac_len = q.len().min(precision);
    out.extend(std::iter::repeat_n('0', precision - frac_len));
    push_digits(&mut out, &q[q.len() - frac_len..]);
    out
}

/// `%g` of a finite magnitude before trailing-zero removal: the body without sign.
fn body_g(exact: Option<&Exact>, precision: usize, upper: bool) -> String {
    let p = precision.max(1);
    let x = match exact {
        None => 0,
        Some(ex) => {
            let q = round_to_unit(ex, ex.exp10 - p as i32);
            if q.len() > p { ex.exp10 } else { ex.exp10 - 1 }
        }
    };
    if (p as i32) > x && x >= -4 {
        body_f(exact, (p as i32 - 1 - x) as usize)
    } else {
        body_e(exact, p - 1, upper)
    }
}

/// The `%g` rule of C17 7.21.6.1p8, without the `#` flag.
fn remove_g_trailing_zeros(body: &mut String) {
    let Some(point) = body.find('.') else {
        return;
    };
    let exp = body.find(['e', 'E']).unwrap_or(body.len());
    let frac_end = body[..exp].trim_end_matches('0').len();
    let frac_end = if frac_end == point + 1 {
        point
    } else {
        frac_end
    };
    body.replace_range(frac_end..exp, "");
}

/// The NaN/infinity spelling, without sign.
fn special_body(crt: Crt, bits: u64, upper: bool) -> &'static str {
    const QUIET: u64 = 1 << 51;
    let mantissa = bits & ((1u64 << 52) - 1);
    let negative = bits >> 63 == 1;
    let (lower, capital) = if mantissa == 0 {
        ("inf", "INF")
    } else {
        match crt {
            Crt::Glibc => ("nan", "NAN"),
            // __acrt_fp_classify (corecrt_internal_fltintrn.h:122-150 @ 10.0.22000.0)
            Crt::Ucrt if negative && mantissa == QUIET => ("nan(ind)", "NAN(IND)"),
            Crt::Ucrt if mantissa & QUIET != 0 => ("nan", "NAN"),
            Crt::Ucrt => ("nan(snan)", "NAN(SNAN)"),
        }
    };
    if upper { capital } else { lower }
}
