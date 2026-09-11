use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

#[test]
fn checked_stop_closes_both_directions_of_an_established_private_tunnel() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut upstream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut remote, _) = listener.accept().unwrap();
    let (mut downstream, mut app) = UnixStream::pair().unwrap();
    for stream in [&upstream, &remote] {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    downstream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    app.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let lifetime = Arc::new(Lifetime::default());
    let tunnel = lifetime.track(&downstream).unwrap();
    tunnel.upstream(&upstream).unwrap();
    let mut outgoing = upstream.try_clone().unwrap();
    let mut incoming = downstream.try_clone().unwrap();
    let (completed, result) = std::sync::mpsc::channel();
    let first = completed.clone();
    let forward = std::thread::spawn(move || {
        first
            .send(std::io::copy(&mut downstream, &mut outgoing))
            .unwrap();
    });
    let reverse = std::thread::spawn(move || {
        completed
            .send(std::io::copy(&mut upstream, &mut incoming))
            .unwrap();
    });
    app.write_all(b"private-egress").unwrap();
    let mut bytes = [0; 14];
    remote.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"private-egress");
    remote.write_all(b"private-reply!").unwrap();
    app.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"private-reply!");
    lifetime.stop().unwrap();
    for _ in 0..2 {
        assert_eq!(
            result
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap(),
            14
        );
    }
    forward.join().unwrap();
    reverse.join().unwrap();
    assert_eq!(remote.read(&mut bytes).unwrap(), 0);
    assert_eq!(app.read(&mut bytes).unwrap(), 0);
    assert!(tunnel.stopping());
    assert!(lifetime.track(&app).is_err());
    assert!(tunnel.upstream(&remote).is_err());
    drop(tunnel);
    assert!(lifetime.tunnels.lock().unwrap().is_empty());
    lifetime.stop().unwrap();
}
