use std::net::SocketAddr;

use crypto::{
    aead::{AeadDecryptor, AeadEncryptor},
    chacha20poly1305::ChaCha20Poly1305,
    sha2::Sha256,
};
use quiche::{ConnectionId, Header};
use rand::RngCore;

/// This idea is "gefladert" (as we like to say in Austria) from nodejs, because I personally have no clue about crypto bullshit.
///      1. Create a `Vec<u8>` with the octets in the address the timestamp in seconds and the dcid of the connection.
///      2. Generate Random bytes.
///      3. Encrypt the the `Vec<u8>` from step 1 with those random bytes and the servers encryption secret.
///      4. Append the random bytes at the end of the newly encrypted `Vec<u8>`.
pub(crate) fn mint_token(header: &Header, src: &SocketAddr, token_secret: &[u8]) -> Vec<u8> {
    let octets = match src.ip() {
        std::net::IpAddr::V4(ip) => ip.octets().to_vec(),
        std::net::IpAddr::V6(ip) => ip.octets().to_vec(),
    };
    let instant = chrono::Utc::now().timestamp();
    let dcid = header.dcid.to_vec();
    let bytes = [octets, instant.to_be_bytes().to_vec(), dcid].concat();
    let mut random = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut random);
    let mut secret = [0u8; 32];
    crypto::hkdf::hkdf_extract(Sha256::new(), &random, token_secret, &mut secret);
    let mut crypto = ChaCha20Poly1305::new(&secret, &random, &[]);
    let mut tag = [0u8; 16];
    let mut encrypted = bytes.clone();
    crypto.encrypt(&bytes, &mut encrypted, &mut tag);
    [encrypted, tag.to_vec(), random.to_vec()].concat()
}

/// Do the steps that were listed above the `mint_token` function but in reverse.
pub(crate) fn validate_token<'a>(
    token: &Vec<u8>,
    src: &SocketAddr,
    token_secret: &[u8],
) -> Option<ConnectionId<'a>> {
    let random = &token[token.len() - 8..].to_vec();
    let tag = &mut token.clone()[token.len() - 24..token.len() - 8].to_vec();
    let encrypted = &token[..token.len() - 24].to_vec();
    let mut secret = [0u8; 32];
    crypto::hkdf::hkdf_extract(Sha256::new(), random, token_secret, &mut secret);
    let mut crypto = ChaCha20Poly1305::new(&secret, random, &[]);
    let mut decrypted = encrypted.clone();
    crypto.decrypt(encrypted, &mut decrypted, tag);

    let octets = match src.ip() {
        std::net::IpAddr::V4(ip) => ip.octets().to_vec(),
        std::net::IpAddr::V6(ip) => ip.octets().to_vec(),
    };
    let mut timestamp: [u8; 8] = [0; 8];
    timestamp.copy_from_slice(&decrypted[octets.len()..octets.len() + 8]);
    let time: chrono::DateTime<chrono::Utc> = chrono::DateTime::from_naive_utc_and_offset(
        chrono::NaiveDateTime::from_timestamp_opt(i64::from_be_bytes(timestamp), 0).unwrap(),
        chrono::Utc,
    );
    let duration = chrono::Utc::now().signed_duration_since(time);
    if octets == decrypted[..octets.len()] && duration.num_seconds() <= 180 {
        Some(ConnectionId::from_vec(
            decrypted[&octets.len() + 8..].to_vec(),
        ))
    } else {
        None
    }
}
