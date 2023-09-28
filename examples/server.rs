#![allow(clippy::unused_io_amount)]
use std::fs;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_quic::error::Result;
use tokio_quic::QuicListener;

#[tokio::main]
async fn main() -> Result<()> {
    //simple_logger::SimpleLogger::new().init().unwrap();

    let mut listener = QuicListener::bind("127.0.0.1:4433").await?;

    while let Ok(mut connection) = listener.accept().await {
        tokio::spawn(async move {
            let mut stream = connection.incoming().await.unwrap();
            let mut buf = [0; 65535];
            let len = stream.read(&mut buf).await.unwrap();
            let path = String::from_utf8_lossy(&buf[..len]);
            println!("Reading: {path}");
            let string = match fs::read_to_string(path.to_string()) {
                Ok(string) => string,
                Err(err) => err.to_string(),
            };
            stream.write(string.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });
    }
    Ok(())
}
