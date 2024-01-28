use std::net::SocketAddr;

use crypto::{
    aead::{AeadDecryptor, AeadEncryptor},
    chacha20poly1305::ChaCha20Poly1305,
    sha2::Sha256,
};
use quiche::ConnectionId;
use rand::RngCore;

/// This idea is "gefladert" (as we like to say in Austria) from nodejs, because I personally have no clue about crypto bullshit.
///      1. Create a `Vec<u8>` with the octets in the address the timestamp in seconds and the dcid of the connection.
///      2. Generate Random bytes.
///      3. Encrypt the the `Vec<u8>` from step 1 with those random bytes and the servers encryption secret.
///      4. Append the random bytes at the end of the newly encrypted `Vec<u8>`.
pub(crate) fn mint_token(
    dcid: &ConnectionId<'_>,
    src: &SocketAddr,
    token_secret: &[u8],
) -> Vec<u8> {
    let octets = ip_to_octets(&src.ip());
    let instant = chrono::Utc::now().timestamp();
    let dcid = dcid.to_vec();
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
    expiration_duration: Option<i64>,
) -> Option<ConnectionId<'a>> {
    let random = &token[token.len() - 8..].to_vec();
    let tag = &mut token.clone()[token.len() - 24..token.len() - 8].to_vec();
    let encrypted = &token[..token.len() - 24].to_vec();
    let mut secret = [0u8; 32];
    crypto::hkdf::hkdf_extract(Sha256::new(), random, token_secret, &mut secret);
    let mut crypto = ChaCha20Poly1305::new(&secret, random, &[]);
    let mut decrypted = encrypted.clone();
    crypto.decrypt(encrypted, &mut decrypted, tag);

    let octets = ip_to_octets(&src.ip());
    let mut timestamp: [u8; 8] = [0; 8];
    timestamp.copy_from_slice(&decrypted[octets.len()..octets.len() + 8]);
    let time: chrono::DateTime<chrono::Utc> = chrono::DateTime::from_naive_utc_and_offset(
        chrono::NaiveDateTime::from_timestamp_opt(i64::from_be_bytes(timestamp), 0).unwrap(),
        chrono::Utc,
    );
    let duration = chrono::Utc::now().signed_duration_since(time);
    if octets == decrypted[..octets.len()]
        && duration.num_seconds() <= expiration_duration.unwrap_or(i64::MAX)
    {
        Some(ConnectionId::from_vec(
            decrypted[&octets.len() + 8..].to_vec(),
        ))
    } else {
        None
    }
}

#[inline]
fn ip_to_octets(ip: &std::net::IpAddr) -> Vec<u8> {
    match ip {
        std::net::IpAddr::V4(ip) => ip.octets().to_vec(),
        std::net::IpAddr::V6(ip) => ip.octets().to_vec(),
    }
}

#[cfg(test)]
mod test {
    use crate::crypto::{mint_token, validate_token};
    use quiche::ConnectionId;
    use std::net::SocketAddr;
    use std::str::FromStr;

    #[test]
    fn validate_token_test() {
        let connection_id = ConnectionId::from_ref(&[
            6, 114, 85, 25, 219, 159, 94, 178, 209, 240, 238, 52, 117, 222, 236, 117,
        ]);
        let token: &[u8] = &[
            115, 35, 157, 90, 216, 123, 207, 22, 84, 203, 149, 220, 123, 81, 54, 156, 226, 47, 232,
            79, 22, 177, 112, 222, 89, 251, 74, 199, 205, 192, 37, 164, 237, 24, 118, 220, 146,
            175, 166, 95, 226, 187, 170, 187, 136, 44, 61, 186, 78, 4, 121, 231,
        ];
        let secret: &[u8] = &[
            193, 225, 35, 100, 179, 123, 28, 109, 213, 167, 40, 242, 57, 91, 85, 30,
        ];
        let socket_addr = SocketAddr::from_str("127.0.0.1:42267").unwrap();

        assert_eq!(
            validate_token(&token.to_vec(), &socket_addr, secret, None),
            Some(connection_id)
        );
    }

    #[test]
    fn mint_token_test() {
        let connection_id = ConnectionId::from_ref(&[
            6, 114, 85, 25, 219, 159, 94, 178, 209, 240, 238, 52, 117, 222, 236, 117,
        ]);
        let secret: &[u8] = &[
            193, 225, 35, 100, 179, 123, 28, 109, 213, 167, 40, 242, 57, 91, 85, 30,
        ];
        let socket_addr = SocketAddr::from_str("127.0.0.1:42267").unwrap();

        let token = mint_token(&connection_id, &socket_addr, secret);
        assert_eq!(
            validate_token(&token, &socket_addr, secret, None),
            Some(connection_id)
        )
    }
}
