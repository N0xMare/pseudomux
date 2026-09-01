#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const FRAGMENTED_ATTACH_CASES: usize = 256;
const ATTACH_STOP_PAYLOAD: &[u8] = b"fragmented-output\r\n\x1b[?1049l";

#[derive(Clone, Default)]
struct SharedOutput(Arc<Mutex<Vec<u8>>>);

impl SharedOutput {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl Write for SharedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_fragmented_attach_data(
    stream: &mut UnixStream,
    client_observer: &UnixStream,
    payload: &[u8],
) {
    stream.write_all(&[1]).unwrap();
    wait_until_fragment_consumed(client_observer, "tag");
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_le_bytes())
        .unwrap();
    wait_until_fragment_consumed(client_observer, "length");
    stream.write_all(payload).unwrap();
    stream.flush().unwrap();
}

#[allow(
    unsafe_code,
    reason = "test queries the unread byte count of its owned Unix socket"
)]
fn wait_until_fragment_consumed(client_observer: &UnixStream, fragment: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut available = 0 as libc::c_int;
        // SAFETY: FIONREAD writes one c_int to the valid local pointer and only
        // inspects the test-owned socket descriptor.
        let result =
            unsafe { libc::ioctl(client_observer.as_raw_fd(), libc::FIONREAD, &mut available) };
        assert_eq!(
            result,
            0,
            "FIONREAD failed after {fragment}: {}",
            std::io::Error::last_os_error()
        );
        if available == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "rmux client did not consume isolated {fragment} fragment"
        );
        std::thread::yield_now();
    }
}

fn read_initial_resize(stream: &mut UnixStream) {
    let mut frame = [0_u8; 5];
    stream.read_exact(&mut frame).unwrap();
    assert_eq!(frame[0], 2, "managed attach must send resize first");
    assert_ne!(u16::from_le_bytes([frame[1], frame[2]]), 0);
    assert_ne!(u16::from_le_bytes([frame[3], frame[4]]), 0);
}

#[allow(unsafe_code, reason = "test owns both fresh openpty descriptors")]
fn open_test_pty() -> (File, File) {
    let mut master = MaybeUninit::<libc::c_int>::uninit();
    let mut slave = MaybeUninit::<libc::c_int>::uninit();
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: output descriptors point to valid storage and become owned only
    // after openpty reports success.
    let result = unsafe {
        libc::openpty(
            master.as_mut_ptr(),
            slave.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &size,
        )
    };
    assert_eq!(
        result,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful openpty returned two fresh owned descriptors.
    let master = unsafe { File::from_raw_fd(master.assume_init()) };
    // SAFETY: successful openpty returned two fresh owned descriptors.
    let slave = unsafe { File::from_raw_fd(slave.assume_init()) };
    (master, slave)
}

#[test]
fn direct_rmux_attach_preserves_fragmented_frame_prefixes() {
    for case in 0..FRAGMENTED_ATTACH_CASES {
        let (client, mut server) = UnixStream::pair().unwrap();
        let client_observer = client.try_clone().unwrap();
        let (input, input_peer) = UnixStream::pair().unwrap();
        let (_resize_tx, resize_rx) = mpsc::channel();
        let output = SharedOutput::default();
        let observed = output.clone();
        let server = std::thread::spawn(move || {
            write_fragmented_attach_data(&mut server, &client_observer, ATTACH_STOP_PAYLOAD);
        });

        let result = rmux_client::drive_attach_stream(client, input, output, resize_rx);
        drop(input_peer);
        server.join().unwrap();
        result.unwrap_or_else(|error| panic!("direct fragmented case {case}: {error}"));
        assert_eq!(observed.bytes(), ATTACH_STOP_PAYLOAD, "direct case {case}");
    }
}

#[test]
fn managed_rmux_attach_preserves_fragmented_frame_prefixes() {
    for case in 0..FRAGMENTED_ATTACH_CASES {
        let (client, mut server) = UnixStream::pair().unwrap();
        let client_observer = client.try_clone().unwrap();
        let (master, terminal) = open_test_pty();
        let input = terminal.try_clone().unwrap();
        let output = SharedOutput::default();
        let observed = output.clone();
        let server = std::thread::spawn(move || {
            read_initial_resize(&mut server);
            write_fragmented_attach_data(&mut server, &client_observer, ATTACH_STOP_PAYLOAD);
        });

        let result = rmux_client::attach_with_terminal(client, &terminal, input, output);
        drop(master);
        drop(terminal);
        server.join().unwrap();
        result.unwrap_or_else(|error| panic!("managed fragmented case {case}: {error}"));
        assert_eq!(observed.bytes(), ATTACH_STOP_PAYLOAD, "managed case {case}");
    }
}
