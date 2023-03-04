/*
 1. On creation generate a new ConnectionID seed.
 2. For each incoming connection check if the data contains a QUIC-Header and extract the ConnectionID.
    Check if we already have this ConnectionID in our CLIENT-MAP and continue, else verify that the packet is a initial and check the version (Version negotiation).
    Then we get the Token from the header, if this is empty we initiate a retry. For this we need to mint a retry-token and send it to the client.
    But if the token is something we need to verify it.
 3. If all checks out we add the connection to our CLIENT-MAP and use the ConnectionID as a key
*/
use std::{collections::HashMap, sync::Arc, io, future::Future, task::{Poll, ready}, time::Instant, pin::Pin};

use quiche::Connection;
use tokio::{sync::{mpsc::{self, UnboundedSender, UnboundedReceiver}, Mutex}, io::ReadBuf, net::UdpSocket};

use crate::{QuicStream, Message, STREAM_BUFFER_SIZE, util::Timer};

use super::{Inner, Server, Driver};

impl Inner<Server> {
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
impl Future for Driver<Server> {
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