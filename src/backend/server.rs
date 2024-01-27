use std::{
    net::SocketAddr,
    sync::Arc,
    task::{ready, Poll},
};

use log::error;
use quiche::Connection;
use tokio::{net::UdpSocket, sync::mpsc::UnboundedReceiver};

use super::{manager::Datapacket, timer::Timer};
use crate::backend::{to_io_error, to_wire, IoHandler};
use crate::error::Result;

#[allow(dead_code)]
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
                    &self.send_buf[self.send_pos..],
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
