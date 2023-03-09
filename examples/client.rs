use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_quic::QuicSocket;

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    //simple_logger::SimpleLogger::new().init();

    let mut connection = QuicSocket::bind("127.0.0.1:0")
        .await?
        .connect(Some("localhost"), "127.0.0.1:4433")
        .await?;

    let mut stream = connection.open().await;
    stream.write(b"./Cargo.toml").await?;
    let mut buf: [u8; u16::MAX as usize] = [0; u16::MAX as usize];
    let n = stream.read(&mut buf).await?;
    println!("{}", String::from_utf8_lossy(&buf[..n]));
    stream.shutdown().await?;
    Ok(())
}
