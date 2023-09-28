use std::ops::{Deref, DerefMut};
use std::{
    collections::HashMap,
    future::Future,
    io,
    sync::Arc,
    task::{ready, Poll},
};

use quiche::Connection;
use tokio::{
    io::ReadBuf,
    net::UdpSocket,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
};

use crate::backend::{to_io_error, to_wire, Driver, IoHandler};
use crate::connection::ToServer;
use crate::{
    error::{Error, Result},
    stream::QuicStream,
    Message, STREAM_BUFFER_SIZE,
};

use super::timer::Timer;

pub(crate) struct Inner {
    pub io: Arc<UdpSocket>,
    pub connection: Connection,
    pub send_flush: bool,
    pub send_end: usize,
    pub send_pos: usize,
    pub recv_buf: Vec<u8>,
    pub send_buf: Vec<u8>,
    pub timer: Timer,
}

impl IoHandler for Inner {
    fn timer(&mut self) -> &mut Timer {
        &mut self.timer
    }

    fn connection(&mut self) -> &mut Connection {
        &mut self.connection
    }

    fn poll_send(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<()>> {
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
            .poll_send(cx, &self.send_buf[self.send_pos..self.send_end]))?;
        self.send_pos += n;

        Poll::Ready(Ok(()))
    }

    fn poll_recv(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<()>> {
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
