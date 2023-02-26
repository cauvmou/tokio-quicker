use std::{future::Future, io, pin::Pin, task::{Poll, ready}};

use quiche::Connection;
use tokio::task::JoinHandle;

pub mod client;
pub mod server;
mod token;

pub trait Driver: Sized {
    fn spawn(mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<(), io::Error>> {
        todo!()
    }
}

pub trait Inner: Send {
    fn connection(&mut self) -> &mut Connection;

    fn poll_io_complete(&mut self, cx: &mut std::task::Context<'_>) -> Poll<Result<Option<()>, io::Error>>;
}

pub struct Handshaker<'a>(pub &'a mut dyn Inner);

impl<'a> Future for Handshaker<'a> {
    type Output = io::Result<()>;

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

