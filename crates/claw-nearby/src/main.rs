//! A bounded non-root transport; control comes only from its inherited pipe.

use clawd_client::nearby::*;
use owly_airdrop::{
    driver::{
        mdns::Mdns,
        receiver::{Options, Receiver, ReceiverEvent, Request},
        sender::{Event, Payload, Sender},
        tls::Identity,
    },
    proto::{
        mdns::{BrowseEvent, Burst, RECEIVER_FLAGS, SUPPORTS_DVZIP, Service},
        messages::Offer,
        receiver::Config,
        sender,
    },
};
use std::{collections::HashMap, io, net::SocketAddrV6, path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{Instant, timeout},
};

fn problem(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

async fn frame<R: AsyncBufRead + Unpin>(
    input: &mut R,
    data: &mut Vec<u8>,
) -> io::Result<Option<WorkerCommand>> {
    loop {
        let chunk = input.fill_buf().await?;
        if chunk.is_empty() {
            return if data.is_empty() {
                Ok(None)
            } else {
                Err(problem("truncated helper frame"))
            };
        }
        let count = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |index| index + 1);
        if data.len() + count > MAX_FRAME_BYTES {
            return Err(problem("helper frame exceeds its bound"));
        }
        data.extend_from_slice(&chunk[..count]);
        input.consume(count);
        if data.last() == Some(&b'\n') {
            let command = serde_json::from_slice(data).map(Some).map_err(problem);
            data.clear();
            return command;
        }
    }
}

fn address(raw: &str, index: u32) -> io::Result<SocketAddrV6> {
    let mut value: SocketAddrV6 = raw.parse().map_err(problem)?;
    if value.ip().segments()[0] & 0xffc0 != 0xfe80 || value.port() == 0 {
        return Err(problem("peer must be link-local IPv6"));
    }
    value.set_scope_id(index);
    Ok(value)
}

fn clean(raw: &str, limit: usize) -> String {
    let result: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'))
        .take(limit / 4)
        .collect();
    if result.trim().is_empty() {
        "Unknown nearby device".into()
    } else {
        result
    }
}

async fn emit(events: &mpsc::Sender<WorkerEvent>, event: WorkerEvent) -> io::Result<()> {
    events.send(event).await.map_err(problem)
}

fn receiver_config(name: String) -> Config {
    let mut config = Config::new(name);
    config.ask_timeout = Duration::from_secs(15);
    config.limits.max_upload_body = MAX_WIRE_BYTES;
    config.limits.extract.max_total = MAX_TOTAL_BYTES;
    config.limits.extract.max_entries = 128;
    config
}

#[tokio::main]
async fn main() -> io::Result<()> {
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return Err(problem("file transport must not run as root"));
    }
    if std::env::args_os().len() != 1 {
        return Err(problem("transport accepts no path or endpoint arguments"));
    }
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| problem("TLS provider already installed"))?;
    let mut input = BufReader::new(tokio::io::stdin());
    let mut command_bytes = Vec::new();
    let Some(WorkerCommand::Start(config)) = frame(&mut input, &mut command_bytes).await? else {
        return Err(problem("missing initial transport configuration"));
    };
    if config.protocol != WORKER_PROTOCOL
        || config.index == 0
        || !text(&config.interface, 32)
        || !text(&config.name, 240)
        || !(30..=600).contains(&config.duration_secs)
    {
        return Err(problem("invalid transport configuration"));
    }
    let link: std::net::Ipv6Addr = config.address.parse().map_err(problem)?;
    if link.segments()[0] & 0xffc0 != 0xfe80 {
        return Err(problem("radio address is not link-local"));
    }
    let (events, mut output) = mpsc::channel::<WorkerEvent>(64);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(event) = output.recv().await {
            let mut data = serde_json::to_vec(&event).map_err(problem)?;
            if data.len() > MAX_FRAME_BYTES {
                return Err(problem("helper event exceeds bound"));
            }
            data.push(b'\n');
            stdout.write_all(&data).await?;
            stdout.flush().await?;
        }
        Ok::<_, io::Error>(())
    });
    let (peer_tx, peer_rx) = watch::channel(Vec::<String>::new());
    let mut requests: HashMap<u64, Request> = HashMap::new();
    let mut receiver_task: Option<JoinHandle<()>> = None;
    let mut receive_events = None;
    let mut registration = None;
    if config.receive {
        let identity = Identity::generate()?;
        let options = receiver_config(config.name.clone());
        let (receiver, rx) = Receiver::bind(
            SocketAddrV6::new(link, 8771, 0, config.index),
            &identity,
            Options::new(options, PathBuf::from("/data/incoming")),
        )
        .await?;
        receiver_task = Some(tokio::spawn(receiver.run()));
        receive_events = Some(rx);
        let burst = config.announce.map(|(count, interval, pause)| Burst {
            count: count.min(64),
            interval: Duration::from_micros(interval.into()),
            pause: Duration::from_micros(pause.into()),
        });
        registration = Some(
            Mdns::new(config.index)
                .with_burst(burst)
                .register(Service {
                    instance: uuid::Uuid::new_v4().simple().to_string()[..12].into(),
                    host: "claw-nearby".into(),
                    port: 8771,
                    addr: link,
                    flags: RECEIVER_FLAGS,
                })
                .await?,
        );
    }
    emit(&events, WorkerEvent::Ready {}).await?;
    let expires = Instant::now() + Duration::from_secs(config.duration_secs.into());
    let mut discover: Option<JoinHandle<()>> = None;
    let mut sending: Option<(String, JoinHandle<()>)> = None;
    let run=async {
        loop {
            tokio::select! {
                _=tokio::time::sleep_until(expires)=>break,
                command=frame(&mut input,&mut command_bytes)=>{
                    match command? {
                        None|Some(WorkerCommand::Stop{})=>break,
                        Some(WorkerCommand::Start(_))=>return Err(problem("duplicate transport configuration")),
                        Some(WorkerCommand::Discover {peers})=>{
                            if peers.len()>MAX_PEERS {return Err(problem("too many radio peers"));}
                            peer_tx.send_replace(peers);
                            if discover.as_ref().is_some_and(|task|!task.is_finished()) {continue;}
                            let events=events.clone();let peers=peer_rx.clone();let config=config.clone();
                            discover=Some(tokio::spawn(async move {
                                if let Err(error)=discover_peers(&config,peers,&events).await {
                                    let _=emit(&events,WorkerEvent::Error {message:clean(&error.to_string(),2048)}).await;
                                }
                                let _=emit(&events,WorkerEvent::DiscoveryFinished{}).await;
                            }));
                        }
                        Some(WorkerCommand::Decide {id,accept})=>{
                            if let Some(request)=requests.remove(&id) {
                                if accept {request.accept();}else{request.decline();}
                            } else {emit(&events,WorkerEvent::Withdrawn{id}).await?;}
                        }
                        Some(WorkerCommand::Send {id,address:raw,flags,files})=>{
                            if !valid_id(&id) || files.is_empty() || files.len()>MAX_FILES
                                || files.iter().any(|path|!valid_send_path(path)) {
                                return Err(problem("invalid send assignment"));
                            }
                            if sending.as_ref().is_some_and(|(_,task)|!task.is_finished()) {
                                return Err(problem("a send is already in flight"));
                            }
                            let address=address(&raw,config.index)?;let events=events.clone();let name=config.name.clone();
                            let transfer_id=id.clone();
                            sending=Some((id,tokio::spawn(async move {
                                match send_files(&name,&transfer_id,address,flags,files,&events).await {
                                    Ok(outcome)=>{let _=emit(&events,WorkerEvent::Sent{id:transfer_id,outcome}).await;}
                                    Err(error)=>{let _=emit(&events,WorkerEvent::Sent{id:transfer_id,outcome:format!("failed: {}",clean(&error.to_string(),1800))}).await;}
                                }
                            })));
                        }
                        Some(WorkerCommand::Cancel {id})=>{
                            if let Some((held,task))=sending.take() {
                                if held!=id {sending=Some((held,task));return Err(problem("send identity differs"));}
                                if !task.is_finished() {task.abort();}
                                match task.await {
                                    Ok(())=>{},
                                    Err(error) if error.is_cancelled()=>{
                                        emit(&events,WorkerEvent::Sent{id,outcome:"interrupted".into()}).await?;
                                    }
                                    Err(error)=>return Err(problem(format!("send task ended unexpectedly: {error}"))),
                                }
                            }
                        }
                    }
                }
                event=async {match &mut receive_events {Some(rx)=>rx.recv().await,None=>std::future::pending().await}}=>{
                    let Some(event)=event else {return Err(problem("receiver event channel closed"));};
                    match event {
                        ReceiverEvent::Request(request)=>{
                            let id=request.id;
                            let Offer::Files(files)=&request.ask.offer else {request.decline();continue;};
                            let metadata:Vec<_>=files.iter().map(|file|SharedFile{name:file.name.clone(),bytes:file.size}).collect();
                            if files.iter().any(|file|file.is_directory) || validate_files(&metadata).is_err()
                                || requests.len()>=MAX_OFFERS {
                                request.decline();continue;
                            }
                            let sender=clean(&request.ask.sender_computer_name,240);
                            emit(&events,WorkerEvent::Offer{id,sender,files:metadata}).await?;
                            requests.insert(id,request);
                        }
                        ReceiverEvent::Withdrawn{id}=>{
                            requests.remove(&id);emit(&events,WorkerEvent::Withdrawn{id}).await?;
                        }
                        ReceiverEvent::Received{id,paths}=>{
                            requests.remove(&id);
                            let paths=paths.into_iter().map(|path|path.file_name().and_then(|name|name.to_str()).map(str::to_string)
                                .ok_or_else(||problem("received path has no safe name"))).collect::<Result<Vec<_>,_>>()?;
                            emit(&events,WorkerEvent::Received{id,paths}).await?;
                        }
                        ReceiverEvent::Failed{id,reason}=>{
                            requests.remove(&id);emit(&events,WorkerEvent::ReceiveFailed{id,message:clean(&reason,2048)}).await?;
                        }
                        ReceiverEvent::Discovered{..}=>{}
                        ReceiverEvent::Urls{..}=>return Err(problem("URL shares are not enabled by this file-only contract")),
                    }
                }
            }
        }
        Ok::<_,io::Error>(())
    }.await;
    requests.clear();
    drop(registration);
    if let Some(task) = receiver_task {
        task.abort();
        let _ = task.await;
    }
    if let Some(task) = discover {
        task.abort();
        let _ = task.await;
    }
    if let Some((_, task)) = sending {
        task.abort();
        let _ = task.await;
    }
    if let Err(error) = &run {
        let _ = emit(
            &events,
            WorkerEvent::Error {
                message: clean(&error.to_string(), 2048),
            },
        )
        .await;
    }
    emit(&events, WorkerEvent::Stopped {}).await?;
    drop(events);
    writer.await.map_err(problem)??;
    run
}

async fn discover_peers(
    config: &LinkConfig,
    peers: watch::Receiver<Vec<String>>,
    events: &mpsc::Sender<WorkerEvent>,
) -> io::Result<()> {
    let own: std::net::Ipv6Addr = config.address.parse().map_err(problem)?;
    let sender = Sender::new(sender::Config::new(
        config.name.clone(),
        uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
    ))?;
    let mut browse = Mdns::new(config.index).browse().await?;
    let end = Instant::now() + Duration::from_secs(25);
    let mut ticker = tokio::time::interval(Duration::from_millis(750));
    let mut found = std::collections::HashSet::new();
    loop {
        let candidates = tokio::select! {
            _=tokio::time::sleep_until(end)=>return Ok(()),
            _=ticker.tick()=>peers.borrow().iter().map(|peer|(peer.clone(),RECEIVER_FLAGS|SUPPORTS_DVZIP)).collect::<Vec<_>>(),
            event=browse.events.recv()=>{
                match event {Some(BrowseEvent::Found(found))=>vec![(SocketAddrV6::new(found.addr,found.port,0,config.index).to_string(),found.flags)],_=>continue}
            }
        };
        for (raw, flags) in candidates.into_iter().take(MAX_PEERS) {
            let addr = address(&raw, config.index)?;
            if *addr.ip() == own {
                continue;
            }
            if found.contains(&addr) {
                continue;
            }
            if let Ok(Ok(Some(peer))) = timeout(Duration::from_secs(2), sender.discover(addr)).await
            {
                found.insert(addr);
                emit(
                    events,
                    WorkerEvent::Peer {
                        address: addr.to_string(),
                        name: clean(&peer.computer_name, 240),
                        flags,
                    },
                )
                .await?;
            }
            if Instant::now() >= end {
                return Ok(());
            }
        }
    }
}

fn valid_send_path(path: &str) -> bool {
    let path = std::path::Path::new(path);
    let Ok(relative) = path.strip_prefix("/data/outgoing") else {
        return false;
    };
    let mut parts = relative.components();
    let Some(std::path::Component::Normal(transfer)) = parts.next() else {
        return false;
    };
    let Some(std::path::Component::Normal(name)) = parts.next() else {
        return false;
    };
    transfer.to_str().is_some_and(valid_id)
        && name.to_str().is_some_and(file_name)
        && parts.next().is_none()
}

async fn send_files(
    name: &str,
    id: &str,
    addr: SocketAddrV6,
    flags: u32,
    files: Vec<String>,
    events: &mpsc::Sender<WorkerEvent>,
) -> io::Result<String> {
    let sender = Sender::new(sender::Config::new(
        name.to_string(),
        uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
    ))?;
    let (progress, mut rx) = mpsc::unbounded_channel();
    let payload = Payload::Files(files.into_iter().map(PathBuf::from).collect());
    let sending = sender.send(addr, &payload, flags, Some(progress));
    tokio::pin!(sending);
    let mut reported = Instant::now() - Duration::from_secs(1);
    loop {
        tokio::select! {
            outcome=&mut sending=>return Ok(match outcome? {
                sender::Outcome::Delivered=>"delivered".into(),
                sender::Outcome::Declined=>"declined".into(),
                sender::Outcome::Failed(reason)=>format!("failed: {}",clean(&reason,1800)),
            }),
            update=rx.recv()=>if let Some(Event::Progress(progress))=update {
                if progress.sent>progress.total || progress.total>MAX_WIRE_BYTES {return Err(problem("compressed transfer exceeds bound"));}
                if progress.sent==progress.total || reported.elapsed()>=Duration::from_millis(100) {
                    emit(events,WorkerEvent::Progress{id:id.into(),bytes:progress.sent,total:progress.total}).await?;
                    reported=Instant::now();
                }
            }

        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/main.rs"));
}
