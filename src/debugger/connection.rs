//! DAP messages over a child process's pipes or a loopback adapter server.
use super::{Events, Writer, session};
use crate::process::read_content_length_message;
use serde_json::Value;
use std::io::{self, BufReader, Read};
use std::net::{SocketAddr, TcpStream};
use std::process::Child;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) struct Connection {
    pub(super) writer: Writer,
    pub(super) messages: Receiver<Result<Value, String>>,
    pub(super) reader: JoinHandle<()>,
    pub(super) worker: JoinHandle<()>,
}

impl Connection {
    pub(super) fn stdio(child: &mut Child, events: Events) -> io::Result<Self> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("adapter stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("adapter stdout unavailable"))?;
        let stdout = crate::process::CancellablePipe::new(stdout, events.actor.cancel.clone())?;
        let (writer, worker) = Writer::start(stdin, events.clone())?;
        let (sender, messages) = mpsc::sync_channel(32);
        let reader = read_messages(stdout, events, sender)?;
        Ok(Self {
            writer,
            messages,
            reader,
            worker,
        })
    }

    pub(super) fn socket(
        address: SocketAddr,
        child: &mut Child,
        events: Events,
    ) -> io::Result<Self> {
        let deadline = Instant::now() + super::ADAPTER_ANSWER_TIMEOUT;
        let socket = loop {
            if events.actor.cancel.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
                Ok(socket) => break socket,
                Err(error) if Instant::now() >= deadline || super::adapter_has_exited(child) => {
                    return Err(error);
                }
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        };
        socket.set_nodelay(true)?;
        socket.set_nonblocking(true)?;
        let input = socket.try_clone()?;
        let input = crate::process::CancellablePipe::new(input, events.actor.cancel.clone())?;
        let (writer, worker) = Writer::socket(socket, events.clone())?;
        let (sender, messages) = mpsc::sync_channel(32);
        let reader = read_messages(input, events, sender)?;
        Ok(Self {
            writer,
            messages,
            reader,
            worker,
        })
    }
}

fn read_messages(
    input: impl Read + Send + 'static,
    events: Events,
    sender: mpsc::SyncSender<Result<Value, String>>,
) -> io::Result<JoinHandle<()>> {
    let reader = thread::Builder::new()
        .name("dust-dap-reader".into())
        .spawn(move || {
            let mut input = BufReader::new(input);
            loop {
                let value = match read_content_length_message(&mut input) {
                    Ok(Some(value)) => Ok(value),
                    Ok(None) => break,
                    Err(error) => Err(error.to_string()),
                };
                if !session::deliver(&sender, value, &events.actor.cancel) {
                    break;
                }
            }
        })?;
    Ok(reader)
}
