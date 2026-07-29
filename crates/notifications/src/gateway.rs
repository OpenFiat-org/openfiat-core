//! The one concrete [`NotificationProvider`]: an HTTP client that hands a
//! sealed payload to a registered gateway's endpoint.
//!
//! The node deliberately does *not* do last-mile delivery. It has no SMTP
//! credentials, no SMS aggregator account, no push certificates — and it
//! cannot read the destination anyway, because the wallet sealed that to
//! the gateway. So the node's entire job at this hop is: POST the sealed
//! payload, observe whether the gateway accepted it, and record that.
//! Everything after (rendering to an inbox, retrying an SMS, noticing a
//! bounce) belongs to the gateway, which reports it back separately as a
//! `DeliveryReport`.

use crate::error::NotificationError;
use crate::provider::{NotificationPayload, NotificationProvider};
use openfiat_types::NotificationChannel;
use std::time::Duration;

/// How long one delivery attempt may take end to end (connect, request,
/// response). A gateway is a third-party service the node has no control
/// over; without a hard ceiling here, one unresponsive operator would
/// stall the node's whole tick, and with it gossip and chain polling. Ten
/// seconds is generous for a JSON POST and still far below any tick this
/// node runs on.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Forwards notifications to gateway endpoints over HTTP.
///
/// One instance is shared across every delivery: `reqwest::Client` owns a
/// connection pool, so constructing one per notification would defeat
/// keep-alive and leak sockets under load.
pub struct HttpGateway {
    client: reqwest::Client,
}

impl HttpGateway {
    /// # Panics
    /// Panics if the HTTP client cannot be built (a broken TLS backend or
    /// unreadable system certificate store) — a node that cannot deliver
    /// anything should fail loudly at startup, not silently at the first
    /// notification.
    pub fn new(timeout: Duration) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("failed to build the notification gateway HTTP client"),
        }
    }
}

impl Default for HttpGateway {
    fn default() -> Self {
        Self::new(DEFAULT_TIMEOUT)
    }
}

#[async_trait::async_trait]
impl NotificationProvider for HttpGateway {
    /// Every channel: this adapter is a transport, and the channel-specific
    /// work happens on the far side of the hop.
    fn channels(&self) -> Vec<NotificationChannel> {
        vec![
            NotificationChannel::Email,
            NotificationChannel::Telegram,
            NotificationChannel::Sms,
            NotificationChannel::Push,
            NotificationChannel::Webhook,
        ]
    }

    async fn send(
        &self,
        endpoint: &str,
        payload: &NotificationPayload,
    ) -> Result<(), NotificationError> {
        let body = openfiat_serialization::json::to_bytes(payload)
            .map_err(|_| NotificationError::MalformedEvent)?;

        let response = self
            .client
            .post(endpoint)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| NotificationError::ProviderUnavailable)?;

        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            // The status alone, never the body: a gateway's error text is
            // untrusted input, and this string ends up in logs.
            Err(NotificationError::DeliveryFailed(format!(
                "gateway responded {}",
                status.as_u16()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{NotificationId, NotificationTrigger};
    use openfiat_crypto::{Keypair, seal};
    use openfiat_types::{PeerId, ServiceId};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    /// One received HTTP request, as the server actually saw it.
    struct ReceivedRequest {
        request_line: String,
        headers: Vec<String>,
        body: Vec<u8>,
    }

    /// A real HTTP/1.1 server on an OS-assigned port, serving exactly one
    /// request. Not a mock `NotificationProvider` — the thing under test
    /// is the bytes that leave the process, so the test has to be on the
    /// receiving end of a real socket.
    async fn serve_once(status_line: &'static str) -> (String, oneshot::Receiver<ReceivedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];

            // Read the head, then exactly Content-Length more bytes.
            let head_end = loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break buffer.len();
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(at) = buffer
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|at| at + 4)
                {
                    break at;
                }
            };
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or_default().to_string();
            let headers: Vec<String> = lines
                .filter(|line| !line.is_empty())
                .map(|line| line.to_ascii_lowercase())
                .collect();
            let content_length: usize = headers
                .iter()
                .find_map(|header| header.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            while buffer.len() - head_end < content_length {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            let body = buffer[head_end..].to_vec();

            socket
                .write_all(format!("{status_line}\r\ncontent-length: 0\r\n\r\n").as_bytes())
                .await
                .unwrap();
            socket.flush().await.unwrap();
            let _ = tx.send(ReceivedRequest {
                request_line,
                headers,
                body,
            });
        });

        (format!("http://{addr}/deliver"), rx)
    }

    fn payload(gateway: &Keypair) -> NotificationPayload {
        NotificationPayload {
            notification_id: NotificationId::derive(
                NotificationTrigger::SettlementApproved,
                b"event-1",
                &PeerId::from_bytes(b"wallet-alpha".to_vec()),
            ),
            trigger: NotificationTrigger::SettlementApproved,
            recipient_wallet: PeerId::from_bytes(b"wallet-alpha".to_vec()),
            service_id: ServiceId::new("gw-1"),
            channel: NotificationChannel::Email,
            sealed_destination: seal(&gateway.public_key(), b"user@example.com").unwrap(),
            subject: "Settlement approved".to_string(),
            body: "A settlement involving your wallet has been approved.".to_string(),
        }
    }

    #[tokio::test]
    async fn posts_the_sealed_payload_the_gateway_can_open() {
        let gateway_key = Keypair::generate();
        let (endpoint, received) = serve_once("HTTP/1.1 202 Accepted").await;
        let payload = payload(&gateway_key);

        HttpGateway::new(DEFAULT_TIMEOUT)
            .send(&endpoint, &payload)
            .await
            .unwrap();

        let request = received.await.unwrap();
        assert!(request.request_line.starts_with("POST /deliver "));
        assert!(
            request
                .headers
                .iter()
                .any(|header| header == "content-type: application/json")
        );

        let decoded: NotificationPayload =
            openfiat_serialization::json::from_bytes(&request.body).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(
            openfiat_crypto::open(&gateway_key, &decoded.sealed_destination).unwrap(),
            b"user@example.com",
            "the gateway — and only the gateway — recovers the address"
        );
    }

    /// The whole privacy argument fails if the address is recoverable
    /// from the bytes on the wire, so assert on the wire itself.
    #[tokio::test]
    async fn the_request_body_never_contains_the_plaintext_destination() {
        let gateway_key = Keypair::generate();
        let (endpoint, received) = serve_once("HTTP/1.1 200 OK").await;

        HttpGateway::new(DEFAULT_TIMEOUT)
            .send(&endpoint, &payload(&gateway_key))
            .await
            .unwrap();

        let request = received.await.unwrap();
        assert!(
            !request
                .body
                .windows(16)
                .any(|window| window == b"user@example.com")
        );
    }

    #[tokio::test]
    async fn a_non_2xx_response_is_a_delivery_failure() {
        let gateway_key = Keypair::generate();
        let (endpoint, _received) = serve_once("HTTP/1.1 503 Service Unavailable").await;

        let result = HttpGateway::new(DEFAULT_TIMEOUT)
            .send(&endpoint, &payload(&gateway_key))
            .await;

        assert_eq!(
            result,
            Err(NotificationError::DeliveryFailed(
                "gateway responded 503".to_string()
            ))
        );
    }

    /// A gateway that accepts the connection and then goes silent is the
    /// realistic hang. Without the client timeout this test never returns.
    #[tokio::test]
    async fn a_hung_gateway_times_out_instead_of_stalling_the_node() {
        let gateway_key = Keypair::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Held open for the duration of the test so the accepted socket
        // is never dropped (which would look like a connection reset
        // rather than a hang).
        let held = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let held_for_server = Arc::clone(&held);
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                held_for_server.lock().await.push(socket);
            }
        });

        let started = std::time::Instant::now();
        let result = HttpGateway::new(Duration::from_millis(150))
            .send(&format!("http://{addr}/deliver"), &payload(&gateway_key))
            .await;

        assert_eq!(result, Err(NotificationError::ProviderUnavailable));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the timeout, not the test harness, must be what ended this"
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_is_reported_as_unavailable() {
        let gateway_key = Keypair::generate();
        // Bind and immediately drop, so the port is almost certainly free.
        let addr = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();

        let result = HttpGateway::new(Duration::from_millis(500))
            .send(&format!("http://{addr}/deliver"), &payload(&gateway_key))
            .await;

        assert_eq!(result, Err(NotificationError::ProviderUnavailable));
    }

    #[tokio::test]
    async fn a_malformed_endpoint_is_reported_rather_than_panicking() {
        let gateway_key = Keypair::generate();
        let result = HttpGateway::new(DEFAULT_TIMEOUT)
            .send("not-a-url", &payload(&gateway_key))
            .await;
        assert_eq!(result, Err(NotificationError::ProviderUnavailable));
    }
}
