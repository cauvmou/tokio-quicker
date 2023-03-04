use std::{future::Future, io, pin::Pin, task::{Poll, ready}, sync::Arc, marker::PhantomData, collections::HashMap};

use quiche::Connection;
use tokio::{task::JoinHandle, net::UdpSocket, sync::{Mutex, mpsc::{UnboundedSender, UnboundedReceiver}}};

use crate::{util::Timer, Message, QuicStream};

pub mod client;
pub mod server;
mod token;

pub trait Backend {}
pub struct Client;
impl Backend for Client {}
pub struct Server;
impl Backend for Server {}

pub struct Inner<B: Backend> {
    pub io: Arc<UdpSocket>,
    pub connection: Connection,
    pub send_flush: bool,
    pub send_end: usize,
    pub send_pos: usize,
    pub recv_buf: Vec<u8>,
    pub send_buf: Vec<u8>,
    pub timer: Timer,
    state: PhantomData<B>,
}

impl<B: Backend> Inner<B> {
    pub fn new<T: Backend>(
        io: Arc<UdpSocket>,
        connection: Connection,
        send_flush: bool,
        send_end: usize,
        send_pos: usize,
        recv_buf: Vec<u8>,
        send_buf: Vec<u8>,
        timer: Timer
    ) -> Self {
        Self {
            io,
            connection,
            send_flush,
            send_end,
            send_pos,
            recv_buf,
            send_buf,
            timer,
            state: PhantomData,
        }
    }
}

pub struct Handshaker<'a, B: Backend>(pub &'a mut Inner<B>);

impl<'a> Future for Handshaker<'a, Client> {
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

impl<'a> Future for Handshaker<'a, Server> {
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

pub struct Driver<B: Backend> {
    pub inner: Inner<B>,
    pub stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>>,
    pub stream_next: Arc<Mutex<u64>>,
    pub message_recv: UnboundedReceiver<Message>,
    pub message_send: UnboundedSender<Message>,
    pub incoming_send: UnboundedSender<QuicStream>,
}