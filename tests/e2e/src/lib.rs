//! Shared helpers for oh-my-ime's test clients and end-to-end tests.

pub mod harness;

use std::os::fd::OwnedFd;

use rustix::fs::MemfdFlags;

/// Creates an shm file of `size` bytes whose 32-bit pixel `i` is `pixels(i)`.
///
/// Uses `pwrite` instead of `mmap`, which keeps the test clients free of `unsafe`.
pub fn shm_file(size: usize, pixels: impl Fn(usize) -> u32) -> anyhow::Result<OwnedFd> {
    let fd = rustix::fs::memfd_create("omime-test-shm", MemfdFlags::CLOEXEC)?;
    rustix::fs::ftruncate(&fd, size as u64)?;
    let mut bytes = Vec::with_capacity(size);
    for i in 0..size / 4 {
        bytes.extend_from_slice(&pixels(i).to_le_bytes());
    }
    let mut written = 0;
    while written < bytes.len() {
        written += rustix::io::pwrite(&fd, &bytes[written..], written as u64)?;
    }
    Ok(fd)
}

/// Parses `RRGGBB` into an opaque ARGB8888 pixel.
pub fn parse_color(hex: &str) -> anyhow::Result<u32> {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16)?;
    anyhow::ensure!(
        hex.trim_start_matches('#').len() == 6,
        "colors look like RRGGBB"
    );
    Ok(0xff00_0000 | rgb)
}

/// Quotes a string as JSON.
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parses a string quoted by [`json_string`]. Returns the string and the rest.
pub fn parse_json_string(s: &str) -> Option<(String, &str)> {
    let mut chars = s.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 2..])),
            '\\' => match chars.next()?.1 {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'u' => {
                    let hex: String = (0..4)
                        .filter_map(|_| chars.next().map(|(_, c)| c))
                        .collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_opaque_argb() {
        assert_eq!(parse_color("ffcc00").unwrap(), 0xffff_cc00);
        assert_eq!(parse_color("#000000").unwrap(), 0xff00_0000);
        assert!(parse_color("fff").is_err());
    }

    #[test]
    fn shm_files_hold_the_pixels() {
        let fd = shm_file(16, |i| i as u32).unwrap();
        let mut buf = [0u8; 16];
        rustix::io::pread(&fd, &mut buf, 0).unwrap();
        assert_eq!(&buf[4..8], &1u32.to_le_bytes());
    }

    #[test]
    fn json_strings_round_trip() {
        for s in ["", "hello", "a \"quoted\" \\ word\n", "中文"] {
            let quoted = json_string(s);
            assert_eq!(parse_json_string(&quoted), Some((s.to_owned(), "")));
        }
        assert_eq!(
            parse_json_string("\"ab\" 3 3"),
            Some(("ab".to_owned(), " 3 3"))
        );
    }
}
