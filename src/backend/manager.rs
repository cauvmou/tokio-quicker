use std::{collections::HashMap, future::Future, io, net::SocketAddr, sync::Arc, task::ready};

use log::{error, info, warn};
use quiche::ConnectionId;
use ring::hmac::Key;
use tokio::{
    io::ReadBuf,
    net::UdpSocket,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};

use crate::{
    crypto::{mint_token, validate_token},
    MAX_DATAGRAM_SIZE,
};

pub struct Client {
    pub connection: quiche::Connection,
    pub recv: UnboundedReceiver<Datapacket>,
}

pub struct Datapacket {
    pub from: SocketAddr,
    pub data: Vec<u8>,
}

pub struct Manager {
    io: Arc<UdpSocket>,
    client_map: HashMap<quiche::ConnectionId<'static>, UnboundedSender<Datapacket>>,
    seed: Key,
    secret_sauce: Vec<u8>,
    config: quiche::Config,
    connection_send: UnboundedSender<Client>,
}

impl Manager {
    pub fn new(
        io: Arc<UdpSocket>,
        seed: Key,
        secret_sauce: Vec<u8>,
        config: quiche::Config,
        connection_send: UnboundedSender<Client>,
    ) -> Self {
        info!("NEW MANAGER");
        Self {
            io,
            client_map: HashMap::new(),
            seed,
            secret_sauce,
            config,
            connection_send,
        }
    }
}

impl Future for Manager {
    type Output = Result<(), io::Error>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        info!("Manager is running");
        let mut buffer: Vec<u8> = vec![0; 65535];
        let mut data_buf: Vec<u8> = vec![0; MAX_DATAGRAM_SIZE];
        'driver: loop {
            let buf = &mut ReadBuf::new(&mut buffer);
            let from = ready!(self.io.poll_recv_from(cx, buf))?;

            let hdr = match quiche::Header::from_slice(buf.filled_mut(), quiche::MAX_CONN_ID_LEN) {
                Ok(header) => header,
                Err(err) => {
                    error!("Error while parsing packet header: {:?}", err);
                    continue 'driver;
                }
            };

            let conn_id = ring::hmac::sign(&self.seed, &hdr.dcid);
            let conn_id = &conn_id.as_ref()[..quiche::MAX_CONN_ID_LEN];
            let conn_id: ConnectionId = conn_id.to_vec().into();

            let sender = if !self.client_map.contains_key(&hdr.dcid)
                && !self.client_map.contains_key(&conn_id)
            {
                if hdr.ty != quiche::Type::Initial {
                    error!("Got Initial packet without having a saved connection.");
                    continue 'driver;
                }

                if !quiche::version_is_supported(hdr.version) {
                    warn!("Version negotiation.");
                    let len =
                        quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut data_buf).unwrap();
                    let data_buf = &data_buf[..len];

                    if let Err(err) = ready!(self.io.poll_send_to(cx, data_buf, from)) {
                        error!("Failed to send negotiation: {:?}", err);
                    }

                    continue 'driver;
                }

                let mut scid = [0; quiche::MAX_CONN_ID_LEN];
                scid.copy_from_slice(&conn_id);

                let scid = quiche::ConnectionId::from_ref(&scid);

                let token = hdr.token.as_ref().unwrap();

                // If empty mint new token
                if token.is_empty() {
                    let new_token = mint_token(&hdr, &from, &self.secret_sauce);

                    let len = quiche::retry(
                        &hdr.scid,
                        &hdr.dcid,
                        &scid,
                        &new_token,
                        hdr.version,
                        &mut data_buf,
                    )
                    .unwrap();

                    let data_buf = &data_buf[..len];

                    if let Err(err) = ready!(self.io.poll_send_to(cx, data_buf, from)) {
                        error!("Failed to send negotiation: {:?}", err);
                    }

                    continue 'driver;
                }

                let odcid = validate_token(token, &from, &self.secret_sauce);

                if odcid.is_none() {
                    error!("Invalid address validation token");
                    continue 'driver;
                }

                if scid.len() != hdr.dcid.len() {
                    error!("Invalid destination connection ID");
                    continue 'driver;
                }

                let scid = hdr.dcid.clone();

                let conn = quiche::accept(
                    &scid,
                    odcid.as_ref(),
                    self.io.local_addr()?,
                    from,
                    &mut self.config,
                )
                .unwrap();

                let (tx, rx) = mpsc::unbounded_channel();

                let client = Client {
                    connection: conn,
                    recv: rx,
                };

                if let Err(_) = self.connection_send.send(client) {
                    error!("Failed to send client to thread!");
                    continue 'driver;
                }

                self.client_map.insert(scid.clone(), tx);

                self.client_map.get_mut(&scid).unwrap()
            } else {
                match self.client_map.get_mut(&hdr.dcid) {
                    Some(v) => v,
                    None => self.client_map.get_mut(&conn_id).unwrap(),
                }
            };

            if let Err(_) = sender.send(Datapacket {
                from,
                data: buf.filled_mut().to_vec(),
            }) {
                error!("Failed to send data to thread!");
            }
        }
    }
}
