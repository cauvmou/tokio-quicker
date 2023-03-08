use std::{io, sync::Arc, collections::HashMap, marker::PhantomData};

use tokio::{task::JoinHandle, sync::{Mutex, mpsc::{UnboundedSender, UnboundedReceiver, self}}};

use crate::{backend::{server, client}, Message, stream::QuicStream};

pub trait Backend {}
pub struct Client;
impl Backend for Client {}
pub struct Server;
impl Backend for Server {}

// Handle multiple streams
pub struct QuicConnection<T: Backend + Send> {
    handle: JoinHandle<Result<(), io::Error>>,
    stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>>, // Map each stream to a `Sender`
    stream_next: Arc<Mutex<u64>>,           // Next available stream id
    message_send: UnboundedSender<Message>, // This is passed to each stream.
    incoming_recv: UnboundedReceiver<QuicStream>,
    state: PhantomData<T>
}

impl QuicConnection<Server> {
    pub fn new(inner: server::Inner) -> Self {
        let (message_send, message_recv) = mpsc::unbounded_channel::<Message>();
        let stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stream_next = Arc::new(Mutex::new(1));
        let (incoming_send, incoming_recv) = mpsc::unbounded_channel();

        let driver = server::Driver {
            inner,
            stream_map: stream_map.clone(),
            stream_next: stream_next.clone(),
            message_recv,
            message_send: message_send.clone(),
            incoming_send,
        };
        let handle = tokio::spawn(driver);

        Self {
            handle,
            stream_map,
            stream_next,
            message_send,
            incoming_recv,
            state: PhantomData,
        }
    }

    pub async fn open(&mut self) -> QuicStream {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut next = self.stream_next.lock().await;
        let id = *next << 2;
        let stream = QuicStream {
            id,
            rx,
            tx: self.message_send.clone(),
        };
        let mut map = self.stream_map.lock().await;
        map.insert(id, tx);
        *next += 1;
        stream
    }

    #[inline]
    pub async fn incoming(&mut self) -> Option<QuicStream> {
        self.incoming_recv.recv().await
    }
}

impl QuicConnection<Client> {
    pub fn new(inner: client::Inner) -> Self {
        let (message_send, message_recv) = mpsc::unbounded_channel::<Message>();
        let stream_map: Arc<Mutex<HashMap<u64, UnboundedSender<Result<Message, quiche::Error>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stream_next = Arc::new(Mutex::new(1));
        let (incoming_send, incoming_recv) = mpsc::unbounded_channel();

        let driver = client::Driver {
            inner,
            stream_map: stream_map.clone(),
            stream_next: stream_next.clone(),
            message_recv,
            message_send: message_send.clone(),
            incoming_send,
        };
        let handle = tokio::spawn(driver);

        Self {
            handle,
            stream_map,
            stream_next,
            message_send,
            incoming_recv,
            state: PhantomData,
        }
    }

    #[inline]
    pub async fn incoming(&mut self) -> Option<QuicStream> {
        self.incoming_recv.recv().await
    }

    pub async fn open(&mut self) -> QuicStream {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut next = self.stream_next.lock().await;
        let id = *next << 2 + 1;
        let stream = QuicStream {
            id,
            rx,
            tx: self.message_send.clone(),
        };
        let mut map = self.stream_map.lock().await;
        map.insert(id, tx);
        *next += 1;
        stream
    }
}