// TODO: The UdpSocket that inner uses needs to be sharable somehow...

use backend::{Driver, Inner, Handshaker};
use quiche::{ConnectionId};
use rand::Rng;
use std::{
    collections::HashMap,
    error::Error,
    io,
    sync::Arc,
    task::{Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UdpSocket,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};
use util::Timer;

mod util;
mod server;
mod backend;

pub const MAX_DATAGRAM_SIZE: usize = 1350;
pub const STREAM_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub enum Message {
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

    pub async fn accept(
        &mut self,
        io: UdpSocket,
        scid: &ConnectionId<'_>,
        odcid: Option<&ConnectionId<'_>>,
    ) -> Result<QuicConnection, Box<dyn Error>> {
        let connection = quiche::accept(
            &scid, odcid,
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

        Ok(QuicConnection::new(inner, true))
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