use std::{
    collections::HashMap,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{ready, Poll},
    time::Instant, net::SocketAddr,
};

use log::{error};
use quiche::Connection;
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};

use crate::{stream::QuicStream, Message, STREAM_BUFFER_SIZE};

use super::{timer::Timer, manager::Datapacket};

pub struct Handshaker<'a>(pub &'a mut Inner);

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

pub struct Inner {
    pub io: Arc<UdpSocket>,
    pub connection: quiche::Connection,
    pub data_recv: UnboundedReceiver<Datapacket>,
    pub send_flush: bool,
    pub send_end: usize,
    pub send_pos: usize,
    pub recv_buf: Vec<u8>,
    pub send_buf: Vec<u8>,
    pub timer: Timer,
    last_address: Option<SocketAddr>
}

impl Inner {
    pub fn new(
        io: Arc<UdpSocket>,
        connection: Connection,
        data_recv: UnboundedReceiver<Datapacket>,
        send_flush: bool,
        send_end: usize,
        send_pos: usize,
        recv_buf: Vec<u8>,
        send_buf: Vec<u8>,
        timer: Timer,
    ) -> Self {
        Self {
            io,
            connection,
            data_recv,
            send_flush,
            send_end,
            send_pos,
            recv_buf,
            send_buf,
            timer,
            last_address: None,
        }
    }

    // TODO: THIS SHIT IS HELLA SUS NO CAP FRFR
    pub fn poll_io_complete(
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
        if self.last_address.is_none() {
            return Poll::Pending;
        }

        if self.send_flush {
            while self.send_pos != self.send_end {
                let n = ready!(self.io.poll_send_to(cx, &mut self.send_buf[self.send_pos..], self.last_address.unwrap()))?;
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
                error!("Closing connection: {:?}", err);
                self.connection
                    .close(false, to_wire(err), b"fail")
                    .map_err(to_io_error)?;
                return Poll::Pending;
            }
        }

        let n = ready!(self
            .io
            .poll_send_to(cx, &mut self.send_buf[self.send_pos..self.send_end], self.last_address.unwrap()))?;
        self.send_pos += n;

        Poll::Ready(Ok(()))
    }

    fn poll_recv(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), io::Error>> {
        let Datapacket { from, mut data } = ready!(self.data_recv.poll_recv(cx)).unwrap();
        let info = quiche::RecvInfo {
            from,
            to: self.io.local_addr()?,
        };
        self.last_address = Some(from);
        match self.connection.recv(&mut data, info) {
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

pub struct Driver {
    pub inner: Inner,
    pub stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>>,
    pub stream_next: Arc<Mutex<u64>>,
    pub message_recv: UnboundedReceiver<Message>,
    pub message_send: UnboundedSender<Message>,
    pub incoming_send: UnboundedSender<QuicStream>,
}

// Backend Driver
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
