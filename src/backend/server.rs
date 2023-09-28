use std::{
    collections::HashMap,
    future::Future,
    io,
    net::SocketAddr,
    sync::Arc,
    task::{ready, Poll},
};

use log::error;
use quiche::Connection;
use tokio::{
    net::UdpSocket,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};

use crate::{stream::QuicStream, Message, STREAM_BUFFER_SIZE, error::{Result, Error}};
use crate::backend::IoHandler;

use super::{manager::Datapacket, timer::Timer};

pub(crate) struct Inner {
    pub io: Arc<UdpSocket>,
    pub connection: Connection,
    pub data_recv: UnboundedReceiver<Datapacket>,
    pub send_flush: bool,
    pub send_end: usize,
    pub send_pos: usize,
    pub recv_buf: Vec<u8>,
    pub send_buf: Vec<u8>,
    pub timer: Timer,
    pub last_address: Option<SocketAddr>,
}

impl IoHandler for Inner {
    fn timer(&mut self) -> &mut Timer {
        &mut self.timer
    }

    fn connection(&mut self) -> &mut Connection {
        &mut self.connection
    }

    fn poll_send(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<()>> {
        if self.last_address.is_none() {
            return Poll::Pending;
        }

        if self.send_flush {
            while self.send_pos != self.send_end {
                let n = ready!(self.io.poll_send_to(
                    cx,
                    &mut self.send_buf[self.send_pos..],
                    self.last_address.unwrap()
                ))?;
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

        let n = ready!(self.io.poll_send_to(
            cx,
            &self.send_buf[self.send_pos..self.send_end],
            self.last_address.unwrap()
        ))?;
        self.send_pos += n;

        Poll::Ready(Ok(()))
    }

    fn poll_recv(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<()>> {
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

pub(crate) struct Driver {
    pub inner: Inner,
    pub stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message>>>>>,
    pub message_recv: UnboundedReceiver<Message>,
    pub message_send: UnboundedSender<Message>,
    pub incoming_send: UnboundedSender<QuicStream>,
}

// Backend Driver
impl Future for Driver {
    type Output = Result<()>;

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
                        let _ = tx.send(Err(err.into()));
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
                        let _ = tx.send(Err(err.into()));
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
