//! The NATS JetStream [`EventTransport`] the outbox publishes to (DOSSIER.md §4, D-005;
//! DOMAIN.md §9.1).
//!
//! Each outbox row is published to its subject `q.<tenant>.<aggregate_type>.<type>` with the
//! header `Nats-Msg-Id: <event_id>`, and the publish counts only once JetStream acknowledges
//! it: the stream stores the event, or recognises the id inside its duplicate window and
//! reports the earlier copy. A crash between the acknowledgement and the outbox row being
//! marked published therefore republishes an event the stream already holds, and the stream,
//! not the consumer, discards it.
//!
//! The client speaks the NATS text protocol over one TCP connection (`CONNECT`, `SUB`,
//! `HPUB`, `PING`/`PONG`) and requests replies on a private inbox, so no second messaging
//! library enters the build. It connects lazily, drops the connection on any protocol or I/O
//! failure and reconnects on the next publish; every failure is a [`TransportError`], and the
//! outbox keeps the row for the next pass. TLS is not spoken: a server that requires it is
//! refused at connect.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::error::TransportError;
use crate::outbox::EventTransport;

/// The JetStream stream the runtime publishes into, capturing every `q.>` subject.
pub const EVENT_STREAM: &str = "QUANSIO_EVENTS";

/// The subjects [`EVENT_STREAM`] captures (DOMAIN.md §9.1 `q.<tenant>.<aggregate_type>.<type>`).
pub const EVENT_SUBJECTS: &str = "q.>";

/// How long the stream remembers a `Nats-Msg-Id` to discard a republished event.
pub const DUPLICATE_WINDOW: Duration = Duration::from_secs(120);

/// How long one request waits for its reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Where and how to reach the server: `nats://[user:password@|token@]host:port`.
#[derive(Clone)]
pub struct NatsConfig {
    address: String,
    user: Option<String>,
    password: Option<String>,
    token: Option<String>,
}

impl std::fmt::Debug for NatsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsConfig")
            .field("address", &self.address)
            .field(
                "credentials",
                &(self.user.is_some() || self.token.is_some()),
            )
            .finish()
    }
}

impl NatsConfig {
    /// Parse a `nats://` URL.
    ///
    /// # Errors
    /// Returns [`TransportError::Unavailable`] for another scheme or a URL without a host.
    pub fn parse(url: &str) -> Result<Self, TransportError> {
        let rest = url.strip_prefix("nats://").ok_or_else(|| {
            TransportError::Unavailable("the event transport URL must start with nats://".into())
        })?;
        let rest = rest.trim_end_matches('/');
        let (credentials, address) = match rest.rsplit_once('@') {
            Some((credentials, address)) => (Some(credentials), address),
            None => (None, rest),
        };
        if address.is_empty() {
            return Err(TransportError::Unavailable(
                "the event transport URL names no host".into(),
            ));
        }
        let address = if address.contains(':') {
            address.to_string()
        } else {
            format!("{address}:4222")
        };
        let (mut user, mut password, mut token) = (None, None, None);
        match credentials.map(|credentials| credentials.split_once(':')) {
            Some(Some((name, secret))) => {
                user = Some(name.to_string());
                password = Some(secret.to_string());
            }
            Some(None) => token = credentials.map(str::to_string),
            None => {}
        }
        Ok(Self {
            address,
            user,
            password,
            token,
        })
    }

    /// The `host:port` connected to.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }
}

struct Connection {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    inbox: String,
    next_reply: u64,
}

/// One reply: its headers' status line (empty for a plain `MSG`) and its payload.
struct Reply {
    status: String,
    payload: Vec<u8>,
}

impl Connection {
    async fn open(config: &NatsConfig) -> Result<Self, TransportError> {
        let stream = tokio::time::timeout(REPLY_TIMEOUT, TcpStream::connect(&config.address))
            .await
            .map_err(|_| unavailable(format!("connecting to {} timed out", config.address)))?
            .map_err(|error| unavailable(format!("cannot reach {}: {error}", config.address)))?;
        let (read, writer) = stream.into_split();
        let mut connection = Self {
            reader: BufReader::new(read),
            writer,
            inbox: format!("_INBOX.{}", quansio_core::UlidGenerator::new().generate()),
            next_reply: 0,
        };
        let info = connection.read_line().await?;
        let info: Value = info
            .strip_prefix("INFO ")
            .and_then(|body| serde_json::from_str(body).ok())
            .ok_or_else(|| unavailable(format!("{} is not a NATS server", config.address)))?;
        if info.get("tls_required").and_then(Value::as_bool) == Some(true) {
            return Err(unavailable(
                "the NATS server requires TLS, which this transport does not speak".into(),
            ));
        }
        if info.get("headers").and_then(Value::as_bool) != Some(true) {
            return Err(unavailable(
                "the NATS server does not support headers (Nats-Msg-Id)".into(),
            ));
        }
        let mut connect = json!({
            "verbose": false,
            "pedantic": false,
            "headers": true,
            "no_responders": true,
            "lang": "rust",
            "version": env!("CARGO_PKG_VERSION"),
            "name": "quansio-outbox",
            "protocol": 1,
        });
        if let (Some(user), Some(password)) = (&config.user, &config.password) {
            connect["user"] = json!(user);
            connect["pass"] = json!(password);
        }
        if let Some(token) = &config.token {
            connect["auth_token"] = json!(token);
        }
        let subscribe = format!(
            "CONNECT {connect}\r\nSUB {}.* 1\r\nPING\r\n",
            connection.inbox
        );
        connection.write(subscribe.as_bytes()).await?;
        loop {
            let line = connection.read_line().await?;
            match line.as_str() {
                "PONG" => return Ok(connection),
                "PING" => connection.write(b"PONG\r\n").await?,
                "+OK" => {}
                other if other.starts_with("-ERR") => {
                    return Err(unavailable(format!("the NATS server refused us: {other}")))
                }
                _ => {}
            }
        }
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.writer
            .write_all(bytes)
            .await
            .map_err(|error| unavailable(format!("writing to NATS: {error}")))
    }

    async fn read_line(&mut self) -> Result<String, TransportError> {
        let mut line = String::new();
        let read = tokio::time::timeout(REPLY_TIMEOUT, self.reader.read_line(&mut line))
            .await
            .map_err(|_| unavailable("NATS did not answer in time".into()))?
            .map_err(|error| unavailable(format!("reading from NATS: {error}")))?;
        if read == 0 {
            return Err(unavailable("NATS closed the connection".into()));
        }
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }

    async fn read_exact(&mut self, length: usize) -> Result<Vec<u8>, TransportError> {
        // The payload and its trailing CRLF.
        let mut buffer = vec![0; length + 2];
        tokio::time::timeout(REPLY_TIMEOUT, self.reader.read_exact(&mut buffer))
            .await
            .map_err(|_| unavailable("NATS did not answer in time".into()))?
            .map_err(|error| unavailable(format!("reading from NATS: {error}")))?;
        buffer.truncate(length);
        Ok(buffer)
    }

    /// Publish `payload` to `subject` with `headers` and wait for the reply to it.
    async fn request(
        &mut self,
        subject: &str,
        headers: &[(&str, &str)],
        payload: &[u8],
    ) -> Result<Reply, TransportError> {
        self.next_reply += 1;
        let reply_to = format!("{}.{}", self.inbox, self.next_reply);
        let mut frame = Vec::with_capacity(payload.len() + 128);
        if headers.is_empty() {
            frame.extend_from_slice(
                format!("PUB {subject} {reply_to} {}\r\n", payload.len()).as_bytes(),
            );
        } else {
            let mut block = String::from("NATS/1.0\r\n");
            for (name, value) in headers {
                block.push_str(&format!("{name}: {value}\r\n"));
            }
            block.push_str("\r\n");
            frame.extend_from_slice(
                format!(
                    "HPUB {subject} {reply_to} {} {}\r\n",
                    block.len(),
                    block.len() + payload.len()
                )
                .as_bytes(),
            );
            frame.extend_from_slice(block.as_bytes());
        }
        frame.extend_from_slice(payload);
        frame.extend_from_slice(b"\r\n");
        self.write(&frame).await?;
        loop {
            let line = self.read_line().await?;
            let mut parts = line.split(' ');
            match parts.next() {
                Some("PING") => self.write(b"PONG\r\n").await?,
                Some("MSG") => {
                    // MSG <subject> <sid> [reply-to] <#bytes>
                    let fields: Vec<&str> = parts.collect();
                    let length = parse_length(fields.last().copied())?;
                    let body = self.read_exact(length).await?;
                    if fields.first() == Some(&reply_to.as_str()) {
                        return Ok(Reply {
                            status: String::new(),
                            payload: body,
                        });
                    }
                }
                Some("HMSG") => {
                    // HMSG <subject> <sid> [reply-to] <#header bytes> <#total bytes>
                    let fields: Vec<&str> = parts.collect();
                    let total = parse_length(fields.last().copied())?;
                    let header_length =
                        parse_length(fields.get(fields.len().saturating_sub(2)).copied())?;
                    let body = self.read_exact(total).await?;
                    if fields.first() == Some(&reply_to.as_str()) {
                        let split = header_length.min(body.len());
                        let status = String::from_utf8_lossy(&body[..split])
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .trim_start_matches("NATS/1.0")
                            .trim()
                            .to_string();
                        return Ok(Reply {
                            status,
                            payload: body[split..].to_vec(),
                        });
                    }
                }
                Some(error) if error.starts_with("-ERR") => {
                    return Err(unavailable(format!("NATS refused the request: {line}")))
                }
                _ => {}
            }
        }
    }

    /// A JetStream API request; a reply carrying `error` is returned as that error object.
    async fn api(
        &mut self,
        subject: &str,
        body: &Value,
    ) -> Result<Result<Value, Value>, TransportError> {
        let reply = self
            .request(subject, &[], body.to_string().as_bytes())
            .await?;
        if reply.status.starts_with("503") {
            return Err(unavailable(
                "JetStream is not enabled on the NATS server".into(),
            ));
        }
        let value: Value = serde_json::from_slice(&reply.payload)
            .map_err(|error| unavailable(format!("JetStream answered with non-JSON: {error}")))?;
        Ok(match value.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(value),
        })
    }

    /// Make sure [`EVENT_STREAM`] exists and captures [`EVENT_SUBJECTS`].
    async fn ensure_stream(&mut self) -> Result<(), TransportError> {
        let info = self
            .api(&format!("$JS.API.STREAM.INFO.{EVENT_STREAM}"), &json!({}))
            .await?;
        match info {
            Ok(_) => Ok(()),
            Err(error) if error.get("code").and_then(Value::as_u64) == Some(404) => {
                let created = self
                    .api(
                        &format!("$JS.API.STREAM.CREATE.{EVENT_STREAM}"),
                        &json!({
                            "name": EVENT_STREAM,
                            "subjects": [EVENT_SUBJECTS],
                            "storage": "file",
                            "retention": "limits",
                            "duplicate_window": u64::try_from(DUPLICATE_WINDOW.as_nanos()).unwrap_or(u64::MAX),
                        }),
                    )
                    .await?;
                match created {
                    Ok(_) => Ok(()),
                    // Another publisher created it between the two requests.
                    Err(error) if error.get("err_code").and_then(Value::as_u64) == Some(10058) => {
                        Ok(())
                    }
                    Err(error) => Err(unavailable(format!(
                        "cannot create the {EVENT_STREAM} stream: {error}"
                    ))),
                }
            }
            Err(error) => Err(unavailable(format!(
                "cannot read the {EVENT_STREAM} stream: {error}"
            ))),
        }
    }
}

fn parse_length(field: Option<&str>) -> Result<usize, TransportError> {
    field
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| unavailable("NATS sent a malformed message header".into()))
}

fn unavailable(detail: String) -> TransportError {
    TransportError::Unavailable(detail)
}

/// What JetStream said about one published event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishAck {
    /// The stream sequence that holds the event.
    pub sequence: u64,
    /// Whether the stream already held this `Nats-Msg-Id` and discarded the copy.
    pub duplicate: bool,
}

/// The production transport: NATS JetStream with `Nats-Msg-Id` deduplication.
pub struct NatsTransport {
    config: NatsConfig,
    connection: Mutex<Option<Connection>>,
}

impl std::fmt::Debug for NatsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsTransport")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl NatsTransport {
    /// A transport for `config`; nothing is connected until the first publish.
    #[must_use]
    pub fn new(config: NatsConfig) -> Self {
        Self {
            config,
            connection: Mutex::new(None),
        }
    }

    /// Publish one event and return JetStream's acknowledgement.
    ///
    /// # Errors
    /// Returns [`TransportError::Unavailable`] when the server cannot be reached or JetStream
    /// is off, and [`TransportError::Rejected`] when the stream refuses the message.
    pub async fn publish_acknowledged(
        &self,
        subject: &str,
        msg_id: &str,
        payload: &Value,
    ) -> Result<PublishAck, TransportError> {
        let mut guard = self.connection.lock().await;
        if guard.is_none() {
            let mut connection = Connection::open(&self.config).await?;
            connection.ensure_stream().await?;
            *guard = Some(connection);
        }
        let Some(connection) = guard.as_mut() else {
            return Err(unavailable("no NATS connection".into()));
        };
        let body = payload.to_string();
        let outcome = connection
            .request(subject, &[("Nats-Msg-Id", msg_id)], body.as_bytes())
            .await;
        let reply = match outcome {
            Ok(reply) => reply,
            Err(error) => {
                // A broken connection is rebuilt on the next publish.
                *guard = None;
                return Err(error);
            }
        };
        if reply.status.starts_with("503") {
            *guard = None;
            return Err(unavailable(format!(
                "no JetStream stream captures {subject}"
            )));
        }
        let ack: Value = serde_json::from_slice(&reply.payload).map_err(|error| {
            TransportError::Rejected(format!("unreadable acknowledgement: {error}"))
        })?;
        if let Some(error) = ack.get("error") {
            return Err(TransportError::Rejected(error.to_string()));
        }
        let sequence = ack.get("seq").and_then(Value::as_u64).ok_or_else(|| {
            TransportError::Rejected(format!("acknowledgement without a sequence: {ack}"))
        })?;
        Ok(PublishAck {
            sequence,
            duplicate: ack.get("duplicate").and_then(Value::as_bool) == Some(true),
        })
    }
}

#[async_trait]
impl EventTransport for NatsTransport {
    async fn publish(
        &self,
        subject: &str,
        msg_id: &str,
        payload: &Value,
    ) -> Result<(), TransportError> {
        self.publish_acknowledged(subject, msg_id, payload)
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nats_url_names_the_address_and_its_credentials() {
        let plain = NatsConfig::parse("nats://127.0.0.1:54230").expect("plain");
        assert_eq!(plain.address(), "127.0.0.1:54230");
        assert!(plain.user.is_none() && plain.token.is_none());
        let defaulted = NatsConfig::parse("nats://nats.internal/").expect("default port");
        assert_eq!(defaulted.address(), "nats.internal:4222");
        let user = NatsConfig::parse("nats://svc:s3cret@nats:4222").expect("user");
        assert_eq!(user.user.as_deref(), Some("svc"));
        assert_eq!(user.password.as_deref(), Some("s3cret"));
        assert!(
            !format!("{user:?}").contains("s3cret"),
            "credentials never print"
        );
        let token = NatsConfig::parse("nats://t0ken@nats:4222").expect("token");
        assert_eq!(token.token.as_deref(), Some("t0ken"));
    }

    #[test]
    fn another_scheme_or_no_host_is_refused() {
        assert!(NatsConfig::parse("http://nats:4222").is_err());
        assert!(NatsConfig::parse("nats://").is_err());
    }
}
