# tokio-quicker

Async QUIC Listener/Socket for [tokio](https://tokio.rs/) using [quiche](https://github.com/cloudflare/quiche).

## Roadmap

- [x] QuicListener for server use.
- [x] QuicSocket for client use.
- [x] Boringssl key generation (Can be disabled by disabling the default features).
- [ ] Swappable backend (quiche/quinn) for boringssl and openssl support.

## Examples

### Client

First create a `QuicSocket`.
```rust
let mut connection = QuicSocket::bind("127.0.0.1:0")
        .await?
        .connect(Some("localhost"), "127.0.0.1:4433")
        .await?;
```
Then you can start opening new `QuicStream`s or receive incoming ones from the server.
```rust
let mut stream = connection.bidi(1).await?;
```
```rust
let mut stream = connection.incoming().await?;
```
These implement the tokio `AsyncRead` and `AsyncWrite` traits.

### Server

Again create a `QuicListener`.

```rust
let mut listener = QuicListener::bind("127.0.0.1:4433").await?;
```
Then you can use a while loop to accept incoming connection and either handle them directly on the thread or move them to a new one.
```rust
while let Ok(mut connection) = listener.accept().await {
    tokio::spawn(async move {
        let mut stream = connection.incoming().await?;
        ...
        stream.shutdown().await?;
    });
}
```