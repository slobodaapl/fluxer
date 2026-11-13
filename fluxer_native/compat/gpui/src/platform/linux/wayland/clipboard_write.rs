// SPDX-License-Identifier: Apache-2.0
//! Bounded, nonblocking writes for Wayland clipboard transfers.
use std::{
    io::{self, ErrorKind, Write},
    os::fd::{AsFd, AsRawFd},
};
const MAX_BYTES_PER_DISPATCH: usize = 64 * 1024;
pub(super) fn set_nonblocking(fd: impl AsFd) -> io::Result<()> {
    let fd = fd.as_fd().as_raw_fd();
    let flags = loop {
        // SAFETY: the borrowed descriptor remains live throughout this call.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            break flags;
        }
        let error = io::Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted {
            return Err(error);
        }
    };
    loop {
        // SAFETY: F_SETFL accepts these flags on a live descriptor.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
/// Returns true on completion, false when the caller should await the next dispatch.
pub(super) fn write_dispatch(
    writer: &mut impl Write,
    bytes: &[u8],
    written: &mut usize,
) -> io::Result<bool> {
    let end = bytes
        .len()
        .min(written.saturating_add(MAX_BYTES_PER_DISPATCH));
    while *written < end {
        match writer.write(&bytes[*written..end]) {
            Ok(0) => return Err(io::Error::from(ErrorKind::WriteZero)),
            Ok(n) => *written += n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(*written == bytes.len())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        io::Read,
        os::fd::{AsRawFd, FromRawFd},
        time::{Duration, Instant},
    };
    #[test]
    fn dispatch_limits_bytes_and_handles_interruption_and_write_zero() {
        let mut output = Vec::new();
        let data = vec![7; MAX_BYTES_PER_DISPATCH + 10];
        let mut written = 0;
        assert!(!write_dispatch(&mut output, &data, &mut written).unwrap());
        assert_eq!(written, MAX_BYTES_PER_DISPATCH);
        assert!(write_dispatch(&mut output, &data, &mut written).unwrap());
        assert_eq!(output, data);
        struct InterruptOnce(bool);
        impl Write for InterruptOnce {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    Err(ErrorKind::Interrupted.into())
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut count = 0;
        assert!(write_dispatch(&mut InterruptOnce(false), b"abc", &mut count).unwrap());
        assert_eq!(count, 3);
        struct Zero;
        impl Write for Zero {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Ok(0)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            write_dispatch(&mut Zero, b"x", &mut 0).unwrap_err().kind(),
            ErrorKind::WriteZero
        );
    }
    #[test]
    fn stalled_private_pipe_does_not_block_and_eventually_delivers_exact_bytes() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        // SAFETY: pipe2 just returned two uniquely owned valid descriptors.
        let mut reader = unsafe { File::from_raw_fd(fds[0]) };
        let mut writer = unsafe { File::from_raw_fd(fds[1]) };
        let old = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
        assert!(old >= 0);
        set_nonblocking(&writer).unwrap();
        let new = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(new & old, old);
        assert_ne!(new & libc::O_NONBLOCK, 0);
        let input = vec![0x9b; 1024 * 1024];
        let mut written = 0;
        let start = Instant::now();
        // With no reader consuming bytes, this must hit WouldBlock promptly.
        loop {
            let before = written;
            assert!(!write_dispatch(&mut writer, &input, &mut written).unwrap());
            if before == written {
                break;
            }
        }
        assert!(written > 0 && written < input.len());
        assert!(start.elapsed() < Duration::from_secs(1));
        let mut received = Vec::new();
        let mut buffer = [0; 4096];
        while received.len() < input.len() {
            if received.len() < written {
                let n = reader.read(&mut buffer).unwrap();
                assert!(n > 0);
                received.extend_from_slice(&buffer[..n]);
            }
            if written < input.len() {
                write_dispatch(&mut writer, &input, &mut written).unwrap();
            }
        }
        assert_eq!(received, input);
    }
}
