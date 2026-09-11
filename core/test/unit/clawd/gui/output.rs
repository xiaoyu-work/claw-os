use super::*;
use std::io::Write;
use std::os::fd::FromRawFd;

#[test]
fn retirement_drains_queued_output_and_keeps_the_exact_bounded_tail() {
    let mut descriptors = [-1_i32; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let mut write = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[1]) });
    let mut output = Output::start(read).unwrap();
    let bytes: Vec<_> = (0..(4 * RETAINED_BYTES))
        .map(|index| b'a' + (index % 26) as u8)
        .collect();
    write.write_all(&bytes).unwrap();
    drop(write);
    let result = output
        .finish(Instant::now() + Duration::from_secs(2))
        .unwrap();
    assert!(result.error.is_none());
    assert!(result.captured.truncated);
    assert_eq!(
        result.captured.text.as_bytes(),
        &bytes[bytes.len() - RETAINED_BYTES..]
    );
    let again = output.finish(Instant::now()).unwrap();
    assert_eq!(again.captured.text, result.captured.text);
}

#[test]
fn retained_output_writer_does_not_fake_a_live_instance_after_checked_process_retirement() {
    let mut descriptors = [-1_i32; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let mut write = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[1]) });
    let mut output = Output::start(read).unwrap();
    write.write_all(b"last private output").unwrap();
    let report = output
        .finish(Instant::now() + Duration::from_secs(2))
        .unwrap();
    assert_eq!(report.captured.text, "last private output");
    assert!(report.error.is_none());
    drop(write);
}
