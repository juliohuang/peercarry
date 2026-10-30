use base64::{engine::general_purpose::STANDARD, Engine};
use peercarry_login_assist::{
    host::{self, Browser, BrowserCall, Field, Service, Snapshot},
    Rect, Target,
};
use serde_json::{json, Value};
use std::io::Cursor;
use tokio::sync::mpsc;

fn snapshot() -> Snapshot {
    Snapshot {
        target: Target {
            device: "test".into(),
            browser: "test".into(),
            tab: 1,
            document: "doc".into(),
            frame: 0,
            origin: "http://127.0.0.1:5188".into(),
        },
        view: Rect {
            x: 1,
            y: 0,
            width: 1,
            height: 2,
        },
        viewport: Rect {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        },
        fields: vec![Field {
            id: "password".into(),
            kind: "password".into(),
        }],
        revision: "revision-1".into(),
    }
}
#[tokio::test]
async fn real_tls_pairing_authorization_crop_and_handoff_with_synthetic_browser() {
    let (tx, mut rx) = mpsc::channel::<BrowserCall>(1);
    let worker = tokio::spawn(async move {
        let mut fills = 0;
        while let Some(call) = rx.recv().await {
            let result = match call.payload["op"].as_str().unwrap() {
                "fill" => {
                    fills += 1;
                    assert_eq!(fills, 1);
                    assert_eq!(call.payload["value"], "synthetic-only");
                    json!({})
                }
                "capture" => {
                    let image = image::RgbaImage::from_raw(3, 2, (0..24).collect()).unwrap();
                    let mut bytes = Cursor::new(Vec::new());
                    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
                    json!({"png": STANDARD.encode(bytes.into_inner())})
                }
                "revoke" => json!({}),
                _ => panic!("unexpected synthetic browser operation"),
            };
            let _ = call.reply.send(Ok(result));
        }
    });
    let (service, pair_secret) = Service::new(snapshot(), Browser(tx)).ok().unwrap();
    let (listener, cert, addr) = host::listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let router = service.router();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let url = format!("https://{addr}");
    // The default root store must reject this ephemeral certificate.
    assert!(reqwest::Client::new()
        .post(format!("{url}/v1/pair"))
        .json(&json!({"secret":"wrong"}))
        .send()
        .await
        .is_err());
    let client = reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .add_root_certificate(
            reqwest::Certificate::from_der(&STANDARD.decode(cert).unwrap()).unwrap(),
        )
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    assert_eq!(
        client
            .post(format!("{url}/v1/pair"))
            .json(&json!({"secret":"wrong"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let paired: Value = client
        .post(format!("{url}/v1/pair"))
        .json(&json!({"secret":pair_secret}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = paired["token"].as_str().unwrap();
    assert_eq!(
        client
            .post(format!("{url}/v1/pair"))
            .json(&json!({"secret":pair_secret}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let endpoint = format!("{url}/v1/command");
    assert_eq!(
        client
            .post(&endpoint)
            .json(&json!({"op":"state"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(token)
            .header("origin", "https://evil.example")
            .json(&json!({"op":"state"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    async fn command(client: &reqwest::Client, url: &str, token: &str, data: Value) -> Value {
        client
            .post(url)
            .bearer_auth(token)
            .json(&data)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    assert_eq!(
        command(&client, &endpoint, token, json!({"op":"frame","epoch":1})).await["error"],
        "INVALID_STATE"
    );
    assert_eq!(
        command(&client, &endpoint, token, json!({"op":"approve","epoch":1})).await["status"]
            ["phase"],
        "Controlling"
    );
    let fill =
        json!({"op":"fill","epoch":1,"command":1,"field":"password","value":"synthetic-only"});
    assert!(command(&client, &endpoint, token, fill.clone())
        .await
        .get("error")
        .is_none());
    assert_eq!(
        command(&client, &endpoint, token, fill).await["error"],
        "REPLAY"
    );
    let frame = command(&client, &endpoint, token, json!({"op":"frame","epoch":1})).await;
    let image = image::load_from_memory(
        &STANDARD
            .decode(frame["data"]["png"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap()
    .to_rgba8();
    assert_eq!(image.dimensions(), (1, 2));
    assert_eq!(image.into_raw(), [4, 5, 6, 7, 16, 17, 18, 19]);
    let complete = command(
        &client,
        &endpoint,
        token,
        json!({"op":"hand_back","epoch":1}),
    )
    .await;
    assert_eq!(complete["status"]["phase"], "Completed");
    assert_eq!(complete["status"]["ai_resumed"], false);
    assert_eq!(
        command(&client, &endpoint, token, json!({"op":"frame","epoch":2})).await["error"],
        "INVALID_STATE"
    );
    service.revoke().await;
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(token)
            .json(&json!({"op":"state"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    server.abort();
    worker.abort();
}

#[test]
fn native_framing_rejects_oversized_and_truncated_messages() {
    use peercarry_login_assist::native;
    assert!(native::read_message(Cursor::new(u32::MAX.to_le_bytes())).is_err());
    assert!(native::read_message(Cursor::new([2, 0, 0, 0, b'{'])).is_err());
    let mut bytes = Vec::new();
    native::write_message(&mut bytes, &json!({"op":"stop"})).unwrap();
    assert_eq!(
        native::read_message(Cursor::new(bytes)).unwrap()["op"],
        "stop"
    );
}
