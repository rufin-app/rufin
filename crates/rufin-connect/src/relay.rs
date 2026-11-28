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

/// The relay an address names and the token that opens it. A relay that is locked with a
/// shared token is given as its address ending in `?token=…`, the form the relay itself
/// reads; the token goes to the relay and is no part of what devices tell each other.
fn locked(address: &str) -> Result<(iroh::RelayUrl, Option<String>)> {
    let mut url: url::Url = address.parse().context("Invalid Iroh relay address")?;
    let token = url
        .query_pairs()
        .find(|(name, _)| name == "token")
        .map(|(_, token)| token.into_owned())
        .filter(|token| !token.is_empty());
    url.set_query(None);
    url.set_fragment(None);
    Ok((canonical_relay(url.into()), token))
}

/// Iroh's public relays, each under the one name a relay goes by here. A device that knew a
/// relay under two names would connect to it twice, and the relay hears only the newer one.
fn public_relays() -> Vec<iroh::RelayUrl> {
    (RelayMode::Default.relay_map().urls::<Vec<_>>().into_iter())
        .map(canonical_relay)
        .collect()
}

/// Whether `relay` is one of Iroh's public relays.
fn public(relay: &iroh::RelayUrl) -> bool {
    public_relays().contains(&canonical_relay(relay.clone()))
}

/// The relays a device connects through, opened with their token.
pub(crate) fn opened(
    relays: impl IntoIterator<Item = iroh::RelayUrl>,
    token: Option<String>,
) -> RelayMode {
    let relays = RelayMode::custom(relays).relay_map();
    RelayMode::Custom(match token {
        Some(token) => relays.with_auth_token(token),
        None => relays,
    })
}

/// The relay selected in Integrations for connections between Rufin devices.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum IrohRelay {
    #[default]
    Off,
    /// Iroh's public relays.
    Public,
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

    /// The choice that names the relay a room is reached through, as its invite gives it.
    pub fn of(relay: &iroh::RelayUrl, token: Option<&str>) -> Self {
        let relay = canonical_relay(relay.clone());
        if public(&relay) {
            return Self::Public;
        }
        let mut url = (*relay).clone();
        if let Some(token) = token {
            url.query_pairs_mut().append_pair("token", token);
        }
        Self::Custom { url: url.into() }
    }

    /// The token that opens the selected relay, for an invite to carry.
    pub fn token(&self) -> Option<String> {
        locked(self.custom_url()?).ok()?.1
    }

    /// The selected custom relay's host, to name it by.
    pub fn host(&self) -> Option<String> {
        let (relay, _) = locked(self.custom_url()?).ok()?;
        relay.host_str().map(str::to_owned)
    }

    /// Whether both choices connect through the same relay with the same token.
    pub fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Custom { url }, Self::Custom { url: other }) => {
                matches!((locked(url), locked(other)), (Ok(one), Ok(other)) if one == other)
            }
            _ => self == other,
        }
    }

    pub(crate) fn custom_media_relay(&self) -> Result<Option<iroh::RelayUrl>> {
        let Some(url) = self.custom_url() else {
            return Ok(None);
        };
        let (custom, _) = locked(url)?;
        // A public relay entered as a custom address stays a public relay: it carries no media.
        Ok((!public(&custom)).then_some(custom))
    }

    pub(crate) fn mode(&self) -> Result<RelayMode> {
        Ok(match self {
            Self::Off => RelayMode::Disabled,
            Self::Public => opened(public_relays(), None),
            Self::Custom { url } => {
                let (relay, token) = locked(url)?;
                opened([relay], token)
            }
        })
    }

    /// Only an explicitly selected custom relay may carry media. The stream stays pinned
    /// through its final acknowledgment, including any retransmissions. Answers whether a
    /// path that may carry it showed up within `wait`.
    pub(crate) async fn send_media(
        &self,
        connection: &Connection,
        send: &mut SendStream,
        response: PeerByteResponse,
        wait: Duration,
    ) -> Result<bool> {
        let custom = self.custom_media_relay()?;
        let permitted = |addr: &TransportAddr| match addr {
            TransportAddr::Ip(_) => true,
            TransportAddr::Relay(url) => custom
                .as_ref()
                .is_some_and(|allowed| allowed == &canonical_relay(url.clone())),
            _ => false,
        };
        let mut paths = connection.paths_stream();
        let path =
            tokio::time::timeout(wait, paths.wait_any(|path| permitted(path.remote_addr()))).await;
        let Ok(Some(path)) = path else {
            send_error(send, "No media connection is available").await?;
            return Ok(false);
        };
        let mut id = path.id();
        send.set_affinity(noq::StreamAffinity::Sticky(id))?;
        let affinity = send.clone();
        let sending = send_response(send, response);
        tokio::pin!(sending);
        loop {
            tokio::select! {
                result = &mut sending => return result.map(|()| true),
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
                IrohRelay::Public,
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
                    let carried = choice
                        .send_media(&tx, &mut send, payload, Duration::from_secs(10))
                        .await
                        .unwrap();
                    assert_eq!(carried, allowed);
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
