use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Poll, ready};
use std::time::Instant;
use quiche::Connection;
use crate::backend::timer::Timer;
use super::error::Result;

pub(crate) mod client;
pub(crate) mod manager;
pub(crate) mod server;
pub(crate) mod timer;

pub(crate) struct Handshaker<'a>(pub &'a mut dyn IoHandler);

impl<'a> Future for Handshaker<'a> {
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

    fn poll_io_complete(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<Option<()>>> {
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