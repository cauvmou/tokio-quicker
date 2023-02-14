use quiche::{Connection, ConnectionId};
use rand::Rng;
use std::{
    collections::HashMap,
    error::Error,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{ready, Poll},
    time::Instant,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UdpSocket,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};
use util::Timer;

mod util;

pub const MAX_DATAGRAM_SIZE: usize = 1350;
pub const STREAM_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug)]
enum Message {
    Data {
        stream_id: u64,
        bytes: Vec<u8>,
        fin: bool,
    },
    Close(u64),
}

// Setup a connection
pub struct QuicSocket {
    config: quiche::Config,
}

impl QuicSocket {
    pub fn new(config: quiche::Config) -> Self {
        Self { config }
    }

    pub async fn connect(
        &mut self,
        io: UdpSocket,
        server_name: Option<&str>,
    ) -> Result<QuicConnection, Box<dyn Error>> {
        let mut scid = vec![0; 16];
        rand::thread_rng().fill(&mut *scid);
        let scid: ConnectionId = scid.into();
        let connection = quiche::connect(
            server_name,
            &scid,
            io.local_addr()?,
            io.peer_addr()?,
            &mut self.config,
        )?;

        let mut inner = Inner {
            io,
            connection,
            send_flush: false,
            send_end: 0,
            send_pos: 0,
            recv_buf: vec![0; STREAM_BUFFER_SIZE],
            send_buf: vec![0; MAX_DATAGRAM_SIZE],
            timer: Timer::Unset,
        };

        let handshake = Handshaker(&mut inner);
        handshake.await?;

        Ok(QuicConnection::new(inner, false))
    }
}

struct Handshaker<'a>(&'a mut Inner);

impl<'a> Future for Handshaker<'a> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        while !self.0.connection.is_established() {
            if let Ok(opt) = ready!(self.0.poll_io_complete(cx)) {
                if opt.is_none() && !self.0.connection.is_established() {
                    return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                }
            }
        }
        Poll::Ready(Ok(()))
    }
}

struct Inner {
    io: UdpSocket,
    connection: Connection,
    send_flush: bool,
    send_end: usize,
    send_pos: usize,
    recv_buf: Vec<u8>,
    send_buf: Vec<u8>,
    timer: Timer,
}

impl Inner {
    fn poll_io_complete(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<()>, io::Error>> {
        if self.timer.ready() {
            self.connection.on_timeout();
        }

        if let Some(timeout) = self.connection.timeout() {
            self.timer = Timer::Set(Instant::now() + timeout)
        } else {
            self.timer = Timer::Unset;
        }

        let recv_result = self.poll_recv(cx)?;
        let send_result = self.poll_send(cx)?;

        match (self.connection.is_closed(), recv_result, send_result) {
            (true, ..) => Poll::Ready(Ok(None)),
            (false, Poll::Pending, Poll::Pending) => Poll::Pending,
            (..) => Poll::Ready(Ok(Some(()))),
        }
    }

    fn poll_send(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), io::Error>> {
        if self.send_flush {
            while self.send_pos != self.send_end {
                let n = ready!(self.io.poll_send(cx, &mut self.send_buf[self.send_pos..]))?;
                self.send_pos += n;
            }

            self.send_pos = 0;
            self.send_end = 0;
            self.send_flush = false;
        }

        match self.connection.send(&mut self.send_buf[self.send_end..]) {
            Ok((n, _info)) => {
                self.send_end += n;
                self.send_flush = self.send_end == self.send_buf.len();
            }
            Err(quiche::Error::Done) if self.send_pos != self.send_end => (),
            Err(quiche::Error::Done) => return Poll::Pending,
            Err(quiche::Error::BufferTooShort) => {
                self.send_flush = true;
                return Poll::Ready(Ok(()));
            }
            Err(err) => {
                self.connection
                    .close(false, to_wire(err), b"fail")
                    .map_err(to_io_error)?;
                return Poll::Pending;
            }
        }

        let n = ready!(self
            .io
            .poll_send(cx, &mut self.send_buf[self.send_pos..self.send_end]))?;
        self.send_pos += n;

        Poll::Ready(Ok(()))
    }

    fn poll_recv(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), io::Error>> {
        let buf = &mut ReadBuf::new(&mut self.recv_buf);
        let from = ready!(self.io.poll_recv_from(cx, buf))?;
        let info = quiche::RecvInfo {
            from,
            to: self.io.local_addr()?,
        };
        match self.connection.recv(buf.filled_mut(), info) {
            Ok(_) => Poll::Ready(Ok(())),
            Err(quiche::Error::Done) => Poll::Ready(Ok(())),
            Err(err) => {
                self.connection
                    .close(false, to_wire(err), b"fail")
                    .map_err(to_io_error)?;
                Poll::Pending
            }
        }
    }
}

// Backend Driver
pub struct Driver {
    inner: Inner,
    stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>>,
    stream_next: Arc<Mutex<u64>>,
    message_recv: UnboundedReceiver<Message>,
    message_send: UnboundedSender<Message>,
    incoming_send: UnboundedSender<QuicStream>,
}

impl Future for Driver {
    type Output = Result<(), io::Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        let mut stream_buf = vec![0; STREAM_BUFFER_SIZE];
        loop {
            // Write Connection
            while let Ok(result) = self.message_recv.try_recv() {
                let (stream_id, result) = match result {
                    Message::Data {
                        stream_id,
                        bytes,
                        fin,
                    } => (
                        stream_id,
                        self.inner.connection.stream_send(stream_id, &bytes, fin),
                    ),
                    Message::Close(stream_id) => (
                        stream_id,
                        self.inner.connection.stream_send(stream_id, &[], true),
                    ),
                };
                if let Err(err) = result {
                    let mut map = pollster::block_on(self.stream_map.lock());
                    if let Some(tx) = map.get_mut(&stream_id) {
                        let _ = tx.send(Err(err));
                    }
                }
            }
            // Read Connection
            for stream_id in self.inner.connection.readable() {
                if self.inner.connection.stream_finished(stream_id) {
                    continue;
                }
                let incoming_send = self.incoming_send.clone();
                let map = self.stream_map.clone();
                let mut map = pollster::block_on(map.lock());

                let mut next = pollster::block_on(self.stream_next.lock());

                let message_send = self.message_send.clone();
                let tx = map.entry(stream_id).or_insert_with(move || {
                    let (tx, rx) = mpsc::unbounded_channel();
                    incoming_send
                        .send(QuicStream {
                            id: stream_id,
                            rx,
                            tx: message_send,
                        })
                        .unwrap();
                    if stream_id >= *next {
                        *next += stream_id + 1;
                    }
                    tx
                });

                match self
                    .inner
                    .connection
                    .stream_recv(stream_id, &mut stream_buf)
                {
                    Ok((len, fin)) => {
                        let _ = tx.send(Ok(Message::Data {
                            stream_id,
                            bytes: stream_buf[..len].to_vec(),
                            fin,
                        }));
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err));
                    }
                }
            }
            // IO
            if let Ok(opt) = ready!(self.inner.poll_io_complete(cx)) {
                if opt.is_none() {
                    return Poll::Ready(Ok(()));
                }
            }
        }
    }
}

// Handle multiple streams
pub struct QuicConnection {
    is_server: bool,
    stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>>, // Map each stream to a `Sender`
    stream_next: Arc<Mutex<u64>>,           // Next available stream id
    message_send: UnboundedSender<Message>, // This is passed to each stream.
    incoming_recv: UnboundedReceiver<QuicStream>,
}

impl QuicConnection {
    fn new(inner: Inner, is_server: bool) -> Self {
        let (message_send, message_recv) = mpsc::unbounded_channel::<Message>();
        let stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stream_next = Arc::new(Mutex::new(1));
        let (incoming_send, incoming_recv) = mpsc::unbounded_channel();

        let driver = Driver {
            inner,
            stream_map: stream_map.clone(),
            stream_next: stream_next.clone(),
            message_recv,
            message_send: message_send.clone(),
            incoming_send,
        };

        tokio::spawn(driver);

        Self {
            is_server,
            stream_map,
            stream_next,
            message_send,
            incoming_recv,
        }
    }

    #[inline]
    pub async fn incoming(&mut self) -> Option<QuicStream> {
        self.incoming_recv.recv().await
    }

    pub async fn open(&mut self) -> QuicStream {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut next = self.stream_next.lock().await;
        let id = *next << 2 + if self.is_server { 1 } else { 0 };
        let stream = QuicStream {
            id,
            rx,
            tx: self.message_send.clone(),
        };
        let mut map = self.stream_map.lock().await;
        map.insert(id, tx);
        *next += 1;
        stream
    }
}

// Readable/Writeable stream
#[derive(Debug)]
pub struct QuicStream {
    id: u64,
    rx: UnboundedReceiver<Result<Message, quiche::Error>>,
    tx: UnboundedSender<Message>,
}

impl QuicStream {
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl AsyncRead for QuicStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(message)) => match message {
                Ok(Message::Data {
                    stream_id: _,
                    bytes,
                    fin: _,
                }) => {
                    buf.put_slice(bytes.as_slice());
                    buf.set_filled(bytes.len());
                    Poll::Ready(Ok(()))
                }
                Ok(Message::Close(_id)) => Poll::Ready(Ok(())),
                Err(err) => {
                    eprintln!("{err}");
                    Poll::Ready(Ok(()))
                }
            },
            Poll::Ready(None) => {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "Whoops")))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let message = Message::Data {
            stream_id: self.id,
            bytes: buf.to_vec(),
            fin: false,
        };
        match self.tx.send(message) {
            Ok(_) => Poll::Ready(Ok(buf.len())),
            Err(err) => Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, err))),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let message = Message::Close(self.id);
        match self.tx.send(message) {
            Ok(_) => Poll::Ready(Ok(())),
            Err(err) => Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, err))),
        }
    }
}

fn to_wire(err: quiche::Error) -> u64 {
    match err {
        quiche::Error::Done => 0x0,
        quiche::Error::InvalidFrame => 0x7,
        quiche::Error::InvalidStreamState(..) => 0x5,
        quiche::Error::InvalidTransportParam => 0x8,
        quiche::Error::FlowControl => 0x3,
        quiche::Error::StreamLimit => 0x4,
        quiche::Error::FinalSize => 0x6,
        _ => 0xa,
    }
}

fn to_io_error(err: quiche::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}
