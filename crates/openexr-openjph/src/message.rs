// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_message.cpp` (external/OpenJPH/src/core/others/ojph_message.cpp:1-181
//! @ OpenJPH 0.32.0) and `ojph_message.h` (external/OpenJPH/src/core/openjph/ojph_message.h:1-292
//! @ OpenJPH 0.32.0): OpenJPH's messages. An error prints
//! `ojph error 0x%08X at <file>:<line>: <text>` to stderr and throws; a warning or an info
//! prints the same to stdout. The file is the source file's name and the line its line in
//! OpenJPH 0.32.0, as upstream's macros give them. The port returns an error where upstream
//! throws.

use std::io::Write;

/// OpenJPH's exceptions: an error's `std::runtime_error ("ojph error")`, with the error's
/// code, or a thrown `const char*` (`what`, with code 0), which the tile's parser reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OjphError {
    pub code: u32,
    pub what: Option<&'static str>,
}

impl OjphError {
    /// `throw "..."`.
    pub(crate) fn thrown(what: &'static str) -> Self {
        OjphError {
            code: 0,
            what: Some(what),
        }
    }
}

impl std::fmt::Display for OjphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.what {
            Some(w) => f.write_str(w),
            None => write!(f, "ojph error 0x{:08X}", self.code),
        }
    }
}

impl std::error::Error for OjphError {}

/// A result of a function that may throw.
pub type OjphResult<T> = Result<T, OjphError>;

/// An argument of a message's `printf` format.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Arg<'a> {
    Int(i64),
    UInt(u64),
    Str(&'a str),
    Double(f64),
}

/// `%e` as the C runtime prints it: six digits after the point, the exponent with a sign and
/// at least two digits.
fn format_e(v: f64) -> String {
    if !v.is_finite() {
        return if v.is_nan() {
            if v.is_sign_negative() {
                "-nan".into()
            } else {
                "nan".into()
            }
        } else if v < 0.0 {
            "-inf".into()
        } else {
            "inf".into()
        };
    }
    let s = format!("{v:.6e}");
    let (mant, exp) = s.split_once('e').expect("an exponent");
    let exp: i32 = exp.parse().expect("an exponent");
    format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

/// `printf` of the formats OpenJPH's messages use: `%d`, `%i`, `%u`, `%s`, `%e`, `%zu` and
/// `%X`/`%08X`.
pub(crate) fn format(fmt: &str, args: &[Arg<'_>]) -> String {
    let mut out = String::new();
    let mut args = args.iter();
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = String::new();
        while let Some(&d) = it.peek() {
            spec.push(d);
            it.next();
            if d.is_ascii_alphabetic() && d != 'z' && d != 'l' {
                break;
            }
            if d == '%' {
                break;
            }
        }
        let conv = spec.chars().last().unwrap_or('%');
        let arg = if conv == '%' {
            None
        } else {
            args.next().copied()
        };
        match (conv, arg) {
            ('%', _) => out.push('%'),
            ('d' | 'i', Some(Arg::Int(v))) => out.push_str(&v.to_string()),
            ('d' | 'i', Some(Arg::UInt(v))) => out.push_str(&(v as i64).to_string()),
            ('u', Some(Arg::UInt(v))) => out.push_str(&v.to_string()),
            ('u', Some(Arg::Int(v))) => out.push_str(&(v as u32).to_string()),
            ('s', Some(Arg::Str(s))) => out.push_str(s),
            ('e', Some(Arg::Double(v))) => out.push_str(&format_e(v)),
            ('X', Some(Arg::UInt(v))) => {
                let w: usize = spec
                    .trim_start_matches('0')
                    .trim_end_matches('X')
                    .parse()
                    .unwrap_or(0);
                out.push_str(&format!("{:0w$X}", v as u32, w = w));
            }
            _ => out.push_str(&spec),
        }
    }
    out
}

/// Port of `message_error::operator()` (external/OpenJPH/src/core/others/ojph_message.cpp:156-171
/// @ OpenJPH 0.32.0) through `OJPH_ERROR`: the message on stderr, and the error.
pub(crate) fn error(code: u32, file: &str, line: u32, fmt: &str, args: &[Arg<'_>]) -> OjphError {
    let text = format(fmt, args);
    let _ = writeln!(
        std::io::stderr(),
        "ojph error 0x{code:08X} at {file}:{line}: {text}"
    );
    OjphError { code, what: None }
}

/// Port of `message_warning::operator()` (external/OpenJPH/src/core/others/ojph_message.cpp:115-128
/// @ OpenJPH 0.32.0) through `OJPH_WARN`: the message on stdout.
pub(crate) fn warn(code: u32, file: &str, line: u32, fmt: &str, args: &[Arg<'_>]) {
    let text = format(fmt, args);
    let _ = writeln!(
        std::io::stdout(),
        "ojph warning 0x{code:08X} at {file}:{line}: {text}"
    );
}

/// Port of `message_info::operator()` (external/OpenJPH/src/core/others/ojph_message.cpp:74-87
/// @ OpenJPH 0.32.0) through `OJPH_INFO`: the message on stdout.
#[allow(dead_code)] // no generic code path informs yet
pub(crate) fn info(code: u32, file: &str, line: u32, fmt: &str, args: &[Arg<'_>]) {
    let text = format(fmt, args);
    let _ = writeln!(
        std::io::stdout(),
        "ojph info 0x{code:08X} at {file}:{line}: {text}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use openexr_testkit::crt;

    fn crt_str(b: Vec<u8>) -> String {
        String::from_utf8(b).unwrap()
    }

    /// The formats OpenJPH's messages use, as the platform's C runtime prints them.
    #[test]
    fn formats_as_the_c_runtime() {
        for v in [0i32, -3, 7, i32::MIN, i32::MAX] {
            assert_eq!(
                format("%d", &[Arg::Int(v.into())]),
                crt_str(crt::snprintf_i32("%d", v))
            );
            assert_eq!(
                format("%i", &[Arg::Int(v.into())]),
                crt_str(crt::snprintf_i32("%i", v))
            );
        }
        for v in [0u32, 7, 0xAB, u32::MAX] {
            assert_eq!(
                format("%u", &[Arg::UInt(v.into())]),
                crt_str(crt::snprintf_u32("%u", v))
            );
            assert_eq!(
                format("%08X", &[Arg::UInt(v.into())]),
                crt_str(crt::snprintf_u32("%08X", v))
            );
        }
        assert_eq!(
            format("%zu", &[Arg::UInt(12)]),
            crt_str(crt::snprintf_usize("%zu", 12))
        );
        for v in [
            0.0,
            -0.0,
            0.00123,
            12345.0,
            1.0 / 3.0,
            2.5e-300,
            1e300,
            f64::INFINITY,
            -f64::INFINITY,
        ] {
            assert_eq!(
                format("%e", &[Arg::Double(v)]),
                crt_str(crt::snprintf_f64("%e", v)),
                "{v}"
            );
        }
        assert_eq!(
            format("%s!", &[Arg::Str("x")]),
            crt_str(crt::snprintf_str("%s!", b"x"))
        );
    }
}
