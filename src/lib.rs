//! Async QUIC Listener/Socket for [tokio](https://tokio.rs/) using [quiche](https://github.com/cloudflare/quiche).
//!
//! ### Examples
//!
//! #### [Client](https://github.com/cauvmou/tokio-quic/blob/main/examples/client.rs)
//!
//! First create a `QuicSocket`.
//! ```rs
//! let mut connection: QuicConnection<Client> = QuicSocket::bind("127.0.0.1:0").await?
//!         .connect(Some("localhost"), "127.0.0.1:4433").await?;
//! ```
//! Then you can start opening new `QuicStream`s or receive incoming ones from the server.
//! ```rs
//! let mut stream: QuicStream = connection.open().await;
//! ```
//! ```rs
//! let mut stream: QuicStream = connection.incoming().await.unwrap();
//! ```
//! These implement the tokio `AsyncRead` and `AsyncWrite` traits.
//!
//! #### [Server](https://github.com/cauvmou/tokio-quic/blob/main/examples/server.rs)
//!
//! Again create a `QuicListener`.
//!
//! ```rs
//! let mut listener: QuicListener = QuicListener::bind("127.0.0.1:4433").await?;
//! ```
//! Then you can use a while loop to accept incoming connection and either handle them directly on the thread or move them to a new one.
//! ```rs
//! while let Ok(mut connection: QuicConnection<Server>) = listener.accept().await {
//!     tokio::spawn(async move {
//!         ...
//!     });
//! }
//! ```

use std::{io, sync::Arc};

use backend::{
    client,
    manager::{self, Manager},
    server,
    timer::Timer,
};
use config::{MAX_DATAGRAM_SIZE, STREAM_BUFFER_SIZE};
use connection::{Client, QuicConnection, Server};
use quiche::ConnectionId;
use rand::Rng;
use ring::rand::SystemRandom;
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    sync::mpsc::{self, UnboundedReceiver},
    task::JoinHandle,
};

mod backend;
pub mod config;
pub mod connection;
mod crypto;
pub mod stream;

#[derive(Debug)]
pub enum Message {
    Data {
        stream_id: u64,
        bytes: Vec<u8>,
        fin: bool,
    },
    Close(u64),
}

/// `QuicListener` is used to bind to a specified address/port.
///
/// It can be configured using a `quiche::Config` struct.
/// A base config can be obtained from `tokio_quic::config::default()`.
///
/// If the feature `key-gen` is enabled this config will already come with a certificate and private key,
/// although these are just for testing and are not recommended to be used in production.
pub struct QuicListener {
    io: Arc<UdpSocket>,
    #[allow(unused)]
    handle: JoinHandle<Result<(), io::Error>>,
    connection_recv: UnboundedReceiver<manager::Client>,
}

impl QuicListener {
    #[cfg(not(feature = "key-gen"))]
    pub async fn bind<A: ToSocketAddrs>(
        addr: A,
        key_pem: &str,
        cert_pem: &str,
    ) -> Result<Self, io::Error> {
        let mut config = config::default();
        config.load_priv_key_from_pem_file(key_pem).unwrap();
        config.load_cert_chain_from_pem_file(cert_pem).unwrap();
        Self::bind_with_config(addr, config).await
    }

    #[cfg(feature = "key-gen")]
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, io::Error> {
        Self::bind_with_config(addr, config::default()).await
    }

    pub async fn bind_with_config<A: ToSocketAddrs>(
        addr: A,
        config: quiche::Config,
    ) -> Result<Self, io::Error> {
        let io = Arc::new(UdpSocket::bind(addr).await?);
        let rng = SystemRandom::new();
        let (tx, connection_recv) = mpsc::unbounded_channel();
        let manager = Manager::new(
            io.clone(),
            ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).unwrap(),
            b"This is a super secret token!".to_vec(),
            config,
            tx,
        );
        let handle = tokio::spawn(manager);
        Ok(Self {
            io,
            handle,
            connection_recv,
        })
    }

    /// Accepts a incoming connection.
    pub async fn accept(&mut self) -> Result<QuicConnection<Server>, io::Error> {
        let manager::Client { connection, recv } = self.connection_recv.recv().await.unwrap();

        let mut inner = server::Inner::new(
            self.io.clone(),
            connection,
            recv,
            false,
            0,
            0,
            vec![0; STREAM_BUFFER_SIZE],
            vec![0; MAX_DATAGRAM_SIZE],
            Timer::Unset,
        );

        server::Handshaker(&mut inner).await?;

        Ok(QuicConnection::<Server>::new(inner))
    }
}

/// `QuicSocket` opens a connection from a specified address/port to a server.
///
/// It can be configured using a `quiche::Config` struct.
/// A base config can be obtained from `tokio_quic::config::default()`.
///
/// If the feature `key-gen` is enabled this config will already come with a certificate and private key,
/// although these are just for testing and are not recommended to be used in production.
pub struct QuicSocket {
    io: Arc<UdpSocket>,
    config: quiche::Config,
}

impl QuicSocket {
    #[cfg(not(feature = "key-gen"))]
    /// Bind to a specified address.
    pub async fn bind<A: ToSocketAddrs>(
        addr: A,
        key_pem: &str,
        cert_pem: &str,
    ) -> Result<Self, io::Error> {
        let mut config = config::default();
        config.load_priv_key_from_pem_file(key_pem).unwrap();
        config.load_cert_chain_from_pem_file(cert_pem).unwrap();
        Self::bind_with_config(addr, config).await
    }

    #[cfg(feature = "key-gen")]
    /// Bind to a specified address.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, io::Error> {
        Self::bind_with_config(addr, config::default()).await
    }

    /// Bind to a specified address with a `quiche::Config`.
    pub async fn bind_with_config<A: ToSocketAddrs>(
        addr: A,
        config: quiche::Config,
    ) -> Result<Self, io::Error> {
        Ok(Self {
            io: Arc::new(UdpSocket::bind(addr).await?),
            config,
        })
    }

    /// Connect to a remote server.
    ///
    /// `server_name` needs to have a value in order to validate the server's certificate.
    /// Can be set to `None`, if validation is turned off.
    pub async fn connect<A: ToSocketAddrs>(
        &mut self,
        server_name: Option<&str>,
        addr: A,
    ) -> Result<QuicConnection<Client>, io::Error> {
        self.io.connect(addr).await?;
        let mut scid = vec![0; 16];
        rand::thread_rng().fill(&mut *scid);
        let scid: ConnectionId = scid.into();
        let connection = quiche::connect(
            server_name,
            &scid,
            self.io.local_addr()?,
            self.io.peer_addr()?,
            &mut self.config,
        )
        .unwrap();

        let mut inner = client::Inner::new(
            self.io.clone(),
            connection,
            false,
            0,
            0,
            vec![0; STREAM_BUFFER_SIZE],
            vec![0; MAX_DATAGRAM_SIZE],
            Timer::Unset,
        );

        client::Handshaker(&mut inner).await?;

        Ok(QuicConnection::<Client>::new(inner))
    }
}
