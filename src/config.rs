pub const MAX_DATAGRAM_SIZE: usize = 1350;
pub const STREAM_BUFFER_SIZE: usize = 64 * 1024;

#[cfg(feature = "key-gen")]
const KEY: &'static [u8] = include_bytes!("./keys/cert.key");
#[cfg(feature = "key-gen")]
const CERT: &'static [u8] = include_bytes!("./keys/cert.crt");

#[cfg(feature = "key-gen")]
pub fn default() -> quiche::Config {
    let mut ctx = boring::ssl::SslContextBuilder::new(boring::ssl::SslMethod::tls()).unwrap();
    let key = boring::pkey::PKey::private_key_from_pem(KEY).unwrap();
    ctx.set_private_key(key.as_ref()).unwrap();
    let x509 = boring::x509::X509::from_pem(CERT).unwrap();
    ctx.set_certificate(x509.as_ref()).unwrap();
    let mut config =
        quiche::Config::with_boring_ssl_ctx(quiche::PROTOCOL_VERSION, ctx.build()).unwrap();
    config
        .set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
        .unwrap();
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
    config
}

#[cfg(not(feature = "key-gen"))]
pub fn default() -> quiche::Config {
    let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
    config
        .set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
        .unwrap();
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
    config
}
