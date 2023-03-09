use boring::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    hash::MessageDigest,
    pkey::PKey,
    rsa::Rsa,
    x509::extension::{AuthorityKeyIdentifier, BasicConstraints, KeyUsage, SubjectKeyIdentifier},
};

fn main() {
    #[cfg(feature = "key-gen")]
    gen_key()
}

fn gen_key() {
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
    name.append_entry_by_text("C", "LM").unwrap();
    name.append_entry_by_text("ST", "AO").unwrap();
    name.append_entry_by_text("O", "something").unwrap();
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

    std::fs::write("./src/keys/cert.key", priv_key).unwrap();
    std::fs::write("./src/keys/cert.crt", x509.build().to_pem().unwrap()).unwrap();
}
