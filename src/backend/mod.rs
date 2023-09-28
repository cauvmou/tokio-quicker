use super::error::Result;
use crate::backend::timer::Timer;
use crate::config::STREAM_BUFFER_SIZE;
use crate::connection::Backend;
use crate::stream::QuicStream;
use crate::Message;
use quiche::Connection;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Poll};
use std::time::Instant;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::{mpsc, Mutex};

pub(crate) mod client;
pub(crate) mod manager;
pub(crate) mod server;
pub(crate) mod timer;

pub(crate) struct Driver<Inner: IoHandler> {
    pub inner: Inner,
    pub stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message>>>>>,
    pub message_recv: UnboundedReceiver<Message>,
    pub message_send: UnboundedSender<Message>,
    pub incoming_send: UnboundedSender<QuicStream>,
}

impl<Inner: IoHandler> Unpin for Driver<Inner> {}

impl<Inner: IoHandler> Future for Driver<Inner> {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
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
                        self.inner.connection().stream_send(stream_id, &bytes, fin),
                    ),
                    Message::Close(stream_id) => (
                        stream_id,
                        self.inner.connection().stream_send(stream_id, &[], true),
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
            for stream_id in self.inner.connection().readable() {
                if self.inner.connection().stream_finished(stream_id) {
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
                    .connection()
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

pub(crate) struct Handshaker<'a, H: IoHandler>(pub &'a mut H);
impl<'a, H: IoHandler> Future for Handshaker<'a, H> {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        while !self.0.connection().is_established() {
            if let Ok(opt) = ready!(self.0.poll_io_complete(cx)) {
                if opt.is_none() && !self.0.connection().is_established() {
                    return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                }
            }
        }
        Poll::Ready(Ok(()))
    }
}

pub(crate) trait IoHandler {
    fn timer(&mut self) -> &mut Timer;

    fn connection(&mut self) -> &mut Connection;

    fn poll_io_complete(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<Option<()>>> {
        if self.timer().ready() {
            self.connection().on_timeout();
        }

        if let Some(timeout) = self.connection().timeout() {
            *self.timer() = Timer::Set(Instant::now() + timeout)
        } else {
            *self.timer() = Timer::Unset;
        }

        let recv_result = self.poll_recv(cx)?;
        let send_result = self.poll_send(cx)?;

        match (self.connection().is_closed(), recv_result, send_result) {
            (true, ..) => Poll::Ready(Ok(None)),
            (false, Poll::Pending, Poll::Pending) => Poll::Pending,
            (..) => Poll::Ready(Ok(Some(()))),
        }
    }

    fn poll_send(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<()>>;

    fn poll_recv(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<()>>;
}

pub(crate) fn to_wire(err: quiche::Error) -> u64 {
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

pub(crate) fn to_io_error(err: quiche::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}
