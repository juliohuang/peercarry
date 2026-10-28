use axum::{http::StatusCode, routing::get, Router};
use peercarry_core::{client::PeerClient, config::Config, error::Error};

#[tokio::test]
async fn hello_keeps_terminal_client_errors_distinct_from_temporary_peer_failures() {
    for status in [
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND,
        StatusCode::BAD_REQUEST,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/v1/hello", get(move || async move { status }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let error = PeerClient::new(&Config::default())
            .unwrap()
            .hello(&address)
            .await
            .unwrap_err();
        server.abort();
        let correct = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                matches!(error, Error::Unauthorized)
            }
            StatusCode::NOT_FOUND => matches!(error, Error::NotFound(_)),
            StatusCode::BAD_REQUEST => matches!(error, Error::Network(_)),
            _ => matches!(error, Error::PeerUnreachable { .. }),
        };
        assert!(correct, "unexpected classification for {status}: {error}");
    }
}
