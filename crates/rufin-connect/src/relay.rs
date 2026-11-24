use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::{
    RelayMode, TransportAddr,
    endpoint::{Connection, SendStream},
};
use serde::{Deserialize, Serialize};

use crate::{
    media::PeerByteResponse,
    network::{canonical_relay, send_error, send_response},
};

/// The relay selected in Integrations for connections between Rufin devices.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum IrohRelay {
    #[default]
    Off,
    Rufin,
    Custom {
        url: String,
    },
}

impl IrohRelay {
    pub fn from_custom(url: Option<String>) -> Self {
        url.map_or(Self::Off, |url| Self::Custom { url })
    }

    pub fn custom_url(&self) -> Option<&str> {
        match self {
            Self::Custom { url } => Some(url),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.mode().map(drop)
    }

    pub(crate) fn custom_media_relay(&self) -> Result<Option<iroh::RelayUrl>> {
        let Some(url) = self.custom_url() else {
            return Ok(None);
        };
        let custom = canonical_relay(url.parse()?);
        // Pasting the published Rufin address as a custom URL keeps its media policy.
        let hosted = option_env!("RUFIN_RELAY_URL")
            .map(str::parse)
            .transpose()?
            .map(canonical_relay);
        Ok((hosted.as_ref() != Some(&custom)).then_some(custom))
    }

    pub(crate) fn mode(&self) -> Result<RelayMode> {
        let url = match self {
            Self::Off => return Ok(RelayMode::Disabled),
            Self::Custom { url } => url.as_str(),
            Self::Rufin => match option_env!("RUFIN_RELAY_URL") {
                Some(url) => url,
                None if app_identity::APP_ID == app_identity::DEVELOPMENT_APP_ID => {
                    return Ok(RelayMode::Default);
                }
                None => anyhow::bail!("The Rufin relay is not configured in this build"),
            },
        };
        let relay: iroh::RelayUrl = url.parse().context("Invalid Iroh relay address")?;
        Ok(RelayMode::custom([canonical_relay(relay)]))
    }

    /// Only an explicitly selected custom relay may carry media. The stream stays pinned
    /// through its final acknowledgment, including any retransmissions.
    pub(crate) async fn send_media(
        &self,
        connection: &Connection,
        send: &mut SendStream,
        response: PeerByteResponse,
    ) -> Result<()> {
        let custom = self.custom_media_relay()?;
        let permitted = |addr: &TransportAddr| match addr {
            TransportAddr::Ip(_) => true,
            TransportAddr::Relay(url) => custom
                .as_ref()
                .is_some_and(|allowed| allowed == &canonical_relay(url.clone())),
            _ => false,
        };
        let mut paths = connection.paths_stream();
        let path = tokio::time::timeout(
            Duration::from_secs(10),
            paths.wait_any(|path| permitted(path.remote_addr())),
        )
        .await;
        let Ok(Some(path)) = path else {
            return send_error(send, "No media connection is available").await;
        };
        let mut id = path.id();
        send.set_affinity(noq::StreamAffinity::Sticky(id))?;
        let affinity = send.clone();
        let sending = send_response(send, response);
        tokio::pin!(sending);
        loop {
            tokio::select! {
                result = &mut sending => return result,
                remaining = paths.wait_closed(id) => {
                    let Some(next) = remaining.and_then(|paths| paths.iter().find(|p| permitted(p.remote_addr()))) else {
                        bail!("Media connection closed");
                    };
                    id = next.id();
                    affinity.set_affinity(noq::StreamAffinity::Sticky(id))?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{MAX_FRAME, read_frame};
    use iroh::{Endpoint, endpoint::presets};
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn only_the_selected_custom_relay_can_carry_a_media_response() {
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, 0)));
        let server = Server::spawn(config).await.unwrap();
        let url: iroh::RelayUrl = format!("http://{}", server.http_addr().unwrap())
            .parse()
            .unwrap();
        let endpoint = || {
            Endpoint::builder(presets::Minimal)
                .clear_ip_transports()
                .relay_mode(RelayMode::custom([url.clone()]))
                .alpns(vec![b"relay-test".to_vec()])
                .bind()
        };
        let (sender, listener) = tokio::try_join!(endpoint(), endpoint()).unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(sender.online(), listener.online());
            let (tx, rx) = tokio::join!(sender.connect(listener.addr(), b"relay-test"), async {
                listener.accept().await.unwrap().await
            });
            let (tx, rx) = (tx.unwrap(), rx.unwrap());
            for choice in [
                IrohRelay::Rufin,
                IrohRelay::Custom {
                    url: url.to_string(),
                },
            ] {
                let allowed = choice.custom_url().is_some();
                let answer = async {
                    let (mut send, _) = tx.accept_bi().await.unwrap();
                    let payload = PeerByteResponse {
                        metadata: serde_json::Value::Null,
                        reader: Box::pin(&b"recording"[..]),
                    };
                    choice.send_media(&tx, &mut send, payload).await.unwrap();
                };
                let read = async {
                    let (mut send, mut recv) = rx.open_bi().await.unwrap();
                    send.write_all(b"play").await.unwrap();
                    send.finish().unwrap();
                    let header: Result<serde_json::Value, String> =
                        read_frame(&mut recv, MAX_FRAME).await.unwrap();
                    let audio = recv.read_to_end(32).await.unwrap();
                    assert_eq!(header.is_ok(), allowed);
                    if allowed {
                        assert_eq!(audio, b"recording");
                    } else {
                        assert!(audio.is_empty());
                    }
                };
                tokio::join!(answer, read);
            }
        })
        .await
        .unwrap();
        tokio::join!(sender.close(), listener.close());
        server.shutdown().await.unwrap();
    }
}
