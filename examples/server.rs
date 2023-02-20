use std::error::Error;

use tokio::net::UdpSocket;
use tokio_quiche::{MAX_DATAGRAM_SIZE, QuicSocket};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;
    config.set_application_protos(quiche::h3::APPLICATION_PROTOCOL)?;
    config.set_max_idle_timeout(5000);
    config.set_max_recv_udp_payload_size(MAX_DATAGRAM_SIZE);
    config.set_max_send_udp_payload_size(MAX_DATAGRAM_SIZE);
    config.set_initial_max_data(10_000_000);
    config.set_initial_max_stream_data_bidi_local(1_000_000);
    config.set_initial_max_stream_data_bidi_remote(1_000_000);
    config.set_initial_max_stream_data_uni(1_000_000);
    config.set_initial_max_streams_bidi(100);
    config.set_initial_max_streams_uni(100);
    config.set_disable_active_migration(true);
    config.load_priv_key_from_pem_file("./localhost.key")?;
    config.load_cert_chain_from_pem_file("./localhost.crt")?;

    let socket = UdpSocket::bind("127.0.0.1:4433").await?;

    Ok(())
}