#[cfg(feature = "key-gen")]
use boring::{asn1::Asn1Time,
bn::{BigNum, MsbOption},
hash::MessageDigest,
pkey::PKey,
rsa::Rsa,
x509::extension::{
    AuthorityKeyIdentifier, BasicConstraints, KeyUsage, SubjectKeyIdentifier,
}};

pub const MAX_DATAGRAM_SIZE: usize = 1350;
pub const STREAM_BUFFER_SIZE: usize = 64 * 1024;

#[cfg(feature = "key-gen")]
pub fn generate_local_certificate() -> (Vec<u8>, Vec<u8>) {
    let rsa = Rsa::generate(2048).unwrap();
    let pkey = PKey::from_rsa(rsa).unwrap();

    let priv_key: Vec<u8> = pkey.private_key_to_pem_pkcs8().unwrap();

    let mut x509 = boring::x509::X509::builder().unwrap();
    x509.set_version(2).unwrap();
    x509.set_pubkey(&pkey).unwrap();

    let serial_number = {
        let mut serial = BigNum::new().unwrap();
        serial.rand(159, MsbOption::MAYBE_ZERO, false).unwrap();
        serial.to_asn1_integer().unwrap()
    };
    x509.set_serial_number(&serial_number).unwrap();

    let mut name = boring::x509::X509Name::builder().unwrap();
    name.append_entry_by_text("CN", "localhost").unwrap();
    let name = name.build();
    x509.set_subject_name(&name).unwrap();
    x509.set_issuer_name(&name).unwrap();

    x509.set_not_after(&Asn1Time::days_from_now(365).unwrap())
        .unwrap();
    x509.set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();

    x509.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
        .unwrap();
    x509.append_extension(
        KeyUsage::new()
            .critical()
            .key_cert_sign()
            .crl_sign()
            .digital_signature()
            .key_encipherment()
            .build()
            .unwrap(),
    )
    .unwrap();

    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&x509.x509v3_context(None, None))
        .unwrap();
    x509.append_extension(subject_key_identifier).unwrap();

    let auth_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(false)
        .issuer(false)
        .build(&x509.x509v3_context(None, None))
        .unwrap();
    x509.append_extension(auth_key_identifier).unwrap();

    x509.sign(&pkey, MessageDigest::sha256()).unwrap();
    (priv_key, x509.build().to_pem().unwrap())
}

#[cfg(feature = "key-gen")]
pub fn default() -> quiche::Config {
    let cert = generate_local_certificate();
    let mut ctx = boring::ssl::SslContextBuilder::new(boring::ssl::SslMethod::tls()).unwrap();
    let key = boring::pkey::PKey::private_key_from_pem(&cert.0).unwrap();
    ctx.set_private_key(key.as_ref()).unwrap();
    let x509 = boring::x509::X509::from_pem(&cert.1).unwrap();
    ctx.set_certificate(x509.as_ref()).unwrap();
    let mut config =
        quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, ctx).unwrap();
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
