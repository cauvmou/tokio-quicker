use std::{io, sync::Arc};

use backend::{manager::{Manager, self}, timer::Timer, server, client};
use config::{STREAM_BUFFER_SIZE, MAX_DATAGRAM_SIZE};
use connection::{QuicConnection, Server, Client};
use quiche::ConnectionId;
use rand::Rng;
use ring::rand::SystemRandom;
use tokio::{net::{ToSocketAddrs, UdpSocket}, sync::mpsc::{self, UnboundedReceiver}, task::JoinHandle};

mod backend;
mod crypto;
mod stream;
mod connection;
pub mod config;

#[derive(Debug)]
pub enum Message {
    Data {
        stream_id: u64,
        bytes: Vec<u8>,
        fin: bool,
    },
    Close(u64),
}

pub struct QuicListener {
    io: Arc<UdpSocket>,
    handle: JoinHandle<Result<(), io::Error>>,
    connection_recv: UnboundedReceiver<manager::Client>,
}

impl QuicListener {
    #[cfg(not(feature = "key-gen"))]
    pub async fn bind<A: ToSocketAddrs>(addr: A, key_pem: &str, cert_pem: &str) -> Result<Self, io::Error> {
        let mut config = config::default();
        config.load_priv_key_from_pem_file(key_pem).unwrap();
        config.load_cert_chain_from_pem_file(cert_pem).unwrap();
        Self::bind_with_config(addr, config).await
    }

    #[cfg(feature = "key-gen")]
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, io::Error> {
        Self::bind_with_config(addr, config::default()).await
    }

    pub async fn bind_with_config<A: ToSocketAddrs>(addr: A, config: quiche::Config) -> Result<Self, io::Error> {
        let io = Arc::new(UdpSocket::bind(addr).await?);
        let rng = SystemRandom::new();
        let (tx, connection_recv) = mpsc::unbounded_channel();
        let manager = Manager::new(
            io.clone(),
            ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).unwrap(),
            b"This is a super secret token!".to_vec(),
            config,
            tx
        );
        let handle = tokio::spawn(manager);
        Ok(Self {
            io,
            handle,
            connection_recv,
        })
    }

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

pub struct QuicSocket {
    io: Arc<UdpSocket>,
    config: quiche::Config,
}

impl QuicSocket {
    #[cfg(not(feature = "key-gen"))]
    pub async fn bind<A: ToSocketAddrs>(addr: A, key_pem: &str, cert_pem: &str) -> Result<Self, io::Error> {
        let mut config = config::default();
        config.load_priv_key_from_pem_file(key_pem).unwrap();
        config.load_cert_chain_from_pem_file(cert_pem).unwrap();
        Self::bind_with_config(addr, config).await
    }

    #[cfg(feature = "key-gen")]
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, io::Error> {
        Self::bind_with_config(addr, config::default()).await
    }

    pub async fn bind_with_config<A: ToSocketAddrs>(addr: A, config: quiche::Config) -> Result<Self, io::Error> {
        Ok(Self {
            io: Arc::new(UdpSocket::bind(addr).await?),
            config
        })
    }

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
        ).unwrap();

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