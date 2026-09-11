use super::*;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::Shutdown;
use std::time::Duration;

use claw_display_control::{workload::SessionGroup, ProcessIdentity};

const DEADLINE: Duration = Duration::from_secs(5);
const RESPONSE_BYTES: usize = MAX_BYTES + MAX_BYTES / 2;
const DESCRIPTOR_BYTES: &[u8] = b"retained GUI descriptor";

fn binding() -> Arc<Binding> {
    let process = ProcessIdentity::current().unwrap();
    let group = SessionGroup::of(&process).expect("a real non-root cgroup is required");
    Binding::receive(process.pidfd().unwrap(), group.open().unwrap())
        .expect("the test process must satisfy the production GUI instance checks")
}

fn wait_for(message: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct RunningRelay {
    state: Arc<State>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl RunningRelay {
    fn start(front: UnixStream, back: UnixStream, descriptors: bool) -> Self {
        let state = Arc::new(State {
            binding: binding(),
            stopped: AtomicBool::new(false),
            active: AtomicUsize::new(1),
            failure: Mutex::new(None),
        });
        let scope = state.clone();
        let worker = std::thread::spawn(move || {
            let _active = Active(scope.clone());
            relay_connected(front, back, &scope, descriptors)
        });
        Self {
            state,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> io::Result<()> {
        wait_for("production relay did not finish", || {
            self.worker.as_ref().unwrap().is_finished()
        });
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for RunningRelay {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.state.stopped.store(true, Ordering::Release);
            wait_for("production relay did not stop during test cleanup", || {
                worker.is_finished()
            });
            let _ = worker.join();
        }
    }
}

fn endpoints() -> (UnixStream, UnixStream, UnixStream, UnixStream) {
    let (client, front) = UnixStream::pair().unwrap();
    crate::clawd::transport::peer::enable_credential_passing(front.as_raw_fd()).unwrap();
    let (back, producer) = UnixStream::pair().unwrap();
    (client, front, back, producer)
}

fn socket_option(socket: &UnixStream, option: i32, value: i32) {
    assert_eq!(
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                std::ptr::addr_of!(value).cast(),
                size_of::<i32>() as libc::socklen_t,
            )
        },
        0,
        "{}",
        io::Error::last_os_error()
    );
}

fn queued_bytes(socket: &UnixStream) -> usize {
    let mut bytes = 0_i32;
    assert_eq!(
        unsafe { libc::ioctl(socket.as_raw_fd(), libc::FIONREAD, &mut bytes) },
        0
    );
    usize::try_from(bytes).unwrap()
}

fn payload() -> Vec<u8> {
    (0..RESPONSE_BYTES)
        .map(|index| (index % 251) as u8)
        .collect()
}

fn queue(socket: &UnixStream, bytes: Vec<u8>, descriptors: Vec<OwnedFd>) {
    socket_option(socket, libc::SO_SNDBUF, (4 * MAX_BYTES) as i32);
    let mut packet = Packet {
        bytes,
        descriptors,
        credentials: None,
        written: 0,
    };
    wait_for("producer could not queue its bounded packet", || {
        send(socket.as_fd(), &mut packet).expect("queue producer packet")
    });
    assert!(packet.descriptors.is_empty());
}

fn fill_outbound(socket: &UnixStream) -> Vec<u8> {
    socket_option(socket, libc::SO_SNDBUF, 4096);
    let mut packet = Packet {
        bytes: vec![0xff; MAX_BYTES],
        descriptors: Vec::new(),
        credentials: None,
        written: 0,
    };
    wait_for("downstream never applied backpressure", || {
        let previous = packet.written;
        assert!(!send(socket.as_fd(), &mut packet).unwrap());
        packet.written == previous
    });
    assert!(packet.written > 0 && packet.written < MAX_BYTES);
    packet.bytes.truncate(packet.written);
    packet.bytes
}

fn drain(socket: &UnixStream) -> (Vec<u8>, Vec<OwnedFd>) {
    let deadline = Instant::now() + DEADLINE;
    let mut bytes = Vec::new();
    let mut descriptors = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "production relay did not deliver EOF"
        );
        match receive(socket.as_fd(), true) {
            Ok(Some(packet)) => {
                bytes.extend(packet.bytes);
                descriptors.extend(packet.descriptors);
                assert!(bytes.len() <= 4 * MAX_BYTES, "unexpected relay output");
            }
            Ok(None) => return (bytes, descriptors),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("read relayed packet: {error}"),
        }
    }
}

fn assert_payload(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len(), "relayed byte count");
    assert_eq!(actual, expected, "relayed byte contents");
}

fn memfd() -> OwnedFd {
    let raw = unsafe { libc::memfd_create(c"gui-proxy-regression".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(raw >= 0, "{}", io::Error::last_os_error());
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(DESCRIPTOR_BYTES).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.into()
}

fn assert_descriptor(mut descriptors: Vec<OwnedFd>) {
    assert_eq!(descriptors.len(), 1, "SCM_RIGHTS must arrive exactly once");
    let descriptor = descriptors.pop().unwrap();
    assert_ne!(
        unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    let mut bytes = Vec::new();
    File::from(descriptor).read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, DESCRIPTOR_BYTES);
}

#[test]
fn producer_close_drains_more_than_one_receive_buffer() {
    let (client, front, back, producer) = endpoints();
    let expected = payload();
    queue(&producer, expected.clone(), Vec::new());
    assert_eq!(queued_bytes(&back), RESPONSE_BYTES);
    drop(producer);
    let relay = RunningRelay::start(front, back, true);
    let (bytes, descriptors) = drain(&client);
    assert_payload(&bytes, &expected);
    assert!(descriptors.is_empty());
    client.shutdown(Shutdown::Write).unwrap();
    relay.finish().unwrap();
}

#[test]
fn producer_close_keeps_partially_written_packet_and_delivers_rights_once() {
    let (client, front, back, producer) = endpoints();
    socket_option(&front, libc::SO_SNDBUF, 4096);
    let expected = payload();
    queue(&producer, expected.clone(), vec![memfd()]);
    drop(producer);
    let relay = RunningRelay::start(front, back, true);
    wait_for("relay never attempted its first downstream write", || {
        queued_bytes(&client) != 0
    });
    assert!(
        queued_bytes(&client) < MAX_BYTES,
        "the first write must be partial"
    );
    let (bytes, descriptors) = drain(&client);
    assert_payload(&bytes, &expected);
    assert_descriptor(descriptors);
    client.shutdown(Shutdown::Write).unwrap();
    relay.finish().unwrap();
}

#[test]
fn producer_close_keeps_unsent_bytes_and_rights_while_downstream_is_full() {
    let (client, front, back, producer) = endpoints();
    let mut expected = fill_outbound(&front);
    let prefix_bytes = expected.len();
    let observed_back = back.try_clone().unwrap();
    queue(&producer, payload(), vec![memfd()]);
    drop(producer);
    let relay = RunningRelay::start(front, back, true);
    wait_for("relay did not receive the queued producer packet", || {
        queued_bytes(&observed_back) < RESPONSE_BYTES
    });
    assert_eq!(queued_bytes(&client), prefix_bytes);
    drop(observed_back);
    expected.extend(payload());
    let (bytes, descriptors) = drain(&client);
    assert_payload(&bytes, &expected);
    assert_descriptor(descriptors);
    client.shutdown(Shutdown::Write).unwrap();
    relay.finish().unwrap();
}

#[test]
fn request_half_close_preserves_reverse_response() {
    let (client, front, back, producer) = endpoints();
    let expected = payload();
    queue(&client, expected.clone(), Vec::new());
    client.shutdown(Shutdown::Write).unwrap();
    let relay = RunningRelay::start(front, back, true);
    let (request, descriptors) = drain(&producer);
    assert_payload(&request, &expected);
    assert!(descriptors.is_empty());
    queue(&producer, expected.clone(), vec![memfd()]);
    drop(producer);
    let (response, descriptors) = drain(&client);
    assert_payload(&response, &expected);
    assert_descriptor(descriptors);
    relay.finish().unwrap();
}

#[test]
fn response_half_close_preserves_later_authenticated_request() {
    let (client, front, back, producer) = endpoints();
    let expected = payload();
    queue(&producer, expected.clone(), Vec::new());
    producer.shutdown(Shutdown::Write).unwrap();
    let relay = RunningRelay::start(front, back, true);
    let (response, descriptors) = drain(&client);
    assert_payload(&response, &expected);
    assert!(descriptors.is_empty());
    queue(&client, expected.clone(), vec![memfd()]);
    client.shutdown(Shutdown::Write).unwrap();
    let (request, descriptors) = drain(&producer);
    assert_payload(&request, &expected);
    assert_descriptor(descriptors);
    relay.finish().unwrap();
}

#[test]
fn simultaneous_half_closes_drain_both_backpressured_directions() {
    let (client, front, back, producer) = endpoints();
    socket_option(&front, libc::SO_SNDBUF, 4096);
    socket_option(&back, libc::SO_SNDBUF, 4096);
    let expected = payload();
    queue(&client, expected.clone(), vec![memfd()]);
    queue(&producer, expected.clone(), vec![memfd()]);
    client.shutdown(Shutdown::Write).unwrap();
    producer.shutdown(Shutdown::Write).unwrap();
    let relay = RunningRelay::start(front, back, true);
    let (response, request) = std::thread::scope(|scope| {
        let response = scope.spawn(|| drain(&client));
        let request = drain(&producer);
        (response.join().unwrap(), request)
    });
    assert_payload(&response.0, &expected);
    assert_descriptor(response.1);
    assert_payload(&request.0, &expected);
    assert_descriptor(request.1);
    relay.finish().unwrap();
}

#[test]
fn checked_stop_retires_backpressured_packet_and_retained_rights() {
    let (client, front, back, producer) = endpoints();
    let prefix = fill_outbound(&front);
    let observed_back = back.try_clone().unwrap();
    let mut pipes = [-1_i32; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipes.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    let mut reader = unsafe { File::from_raw_fd(pipes[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(pipes[1]) };
    queue(&producer, vec![0x55; 1024], vec![writer]);
    let relay = RunningRelay::start(front, back, true);
    wait_for(
        "relay did not take custody of the pending descriptor",
        || queued_bytes(&observed_back) == 0,
    );
    drop(observed_back);
    assert_eq!(
        reader.read(&mut [0_u8]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(queued_bytes(&client), prefix.len());
    let mut proxy = Proxy {
        state: relay.state.clone(),
        listener: None,
    };
    proxy.finish(Instant::now() + DEADLINE).unwrap();
    proxy.health().unwrap();
    assert_eq!(proxy.state.active.load(Ordering::Acquire), 0);
    relay.finish().unwrap();
    assert_eq!(reader.read(&mut [0_u8]).unwrap(), 0);
    assert_payload(&drain(&client).0, &prefix);
    assert!(drain(&producer).0.is_empty());
}

struct SocketPath(PathBuf);

impl Drop for SocketPath {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).unwrap();
    }
}

fn listener() -> (UnixListener, SocketPath) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = PathBuf::from(format!(
        ".gui-proxy-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    (listener, SocketPath(path))
}

#[test]
fn checked_stop_joins_listener_and_idle_connection() {
    let (target, target_path) = listener();
    target.set_nonblocking(true).unwrap();
    let (front, front_path) = listener();
    let mut proxy = Proxy::start(front, target_path.0.clone(), binding(), true).unwrap();
    let client = UnixStream::connect(&front_path.0).unwrap();
    let mut producer = None;
    wait_for(
        "production proxy did not connect to its target",
        || match target.accept() {
            Ok((stream, _)) => {
                producer = Some(stream);
                true
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
            Err(error) => panic!("accept production proxy: {error}"),
        },
    );
    proxy.finish(Instant::now() + DEADLINE).unwrap();
    proxy.health().unwrap();
    assert!(proxy.listener.is_none());
    assert_eq!(proxy.state.active.load(Ordering::Acquire), 0);
    assert!(drain(&client).0.is_empty());
    assert!(drain(&producer.unwrap()).0.is_empty());
    proxy.finish(Instant::now()).unwrap();
}

#[test]
fn relay_rejects_messages_without_kernel_writer_credentials() {
    let (client, front, back, producer) = endpoints();
    socket_option(&front, libc::SO_PASSCRED, 0);
    queue(&client, b"uncredentialed request".to_vec(), Vec::new());
    let relay = RunningRelay::start(front, back, true);
    let error = relay.finish().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("no kernel writer"));
    assert!(drain(&producer).0.is_empty());
}

#[test]
fn relay_rejects_socket_directory_and_process_authority_descriptors() {
    let (socket, _) = UnixStream::pair().unwrap();
    for descriptor in [
        OwnedFd::from(socket),
        File::open(".").unwrap().into(),
        ProcessIdentity::current().unwrap().pidfd().unwrap(),
    ] {
        let (client, front, back, producer) = endpoints();
        queue(&producer, payload(), vec![descriptor]);
        drop(producer);
        let relay = RunningRelay::start(front, back, true);
        assert_eq!(
            relay.finish().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(drain(&client).0.is_empty());
    }
}

#[test]
fn byte_only_relay_rejects_even_ordinary_file_descriptors() {
    let (client, front, back, producer) = endpoints();
    queue(&producer, payload(), vec![memfd()]);
    drop(producer);
    let relay = RunningRelay::start(front, back, false);
    assert_eq!(
        relay.finish().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(drain(&client).0.is_empty());
}
