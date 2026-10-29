//! Shared helpers for oh-my-ime's test clients.

use std::os::fd::OwnedFd;

use rustix::fs::MemfdFlags;

/// Creates an shm file of `size` bytes filled with `data` (repeated as needed).
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
}
