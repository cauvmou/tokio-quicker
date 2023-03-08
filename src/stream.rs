use std::{io, task::Poll};

use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
};

use crate::Message;

// Readable/Writeable stream
#[derive(Debug)]
pub struct QuicStream {
    pub(crate) id: u64,
    pub(crate) rx: UnboundedReceiver<Result<Message, quiche::Error>>,
    pub(crate) tx: UnboundedSender<Message>,
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
