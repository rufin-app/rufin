//! A room's own endpoint, apart from Connect's: the host accepts members and a guest reaches
//! the host. Its key lasts as long as the room, so an invite dies with the room and names
//! nothing of the device's Connect identity. Frames are small: they come from people outside
//! the profile. A member also asks another member for audio here, on a connection of its own.
use std::{sync::Mutex, time::Duration};

use anyhow::{Context, Result};
use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr,
    endpoint::{Connection, QuicTransportConfig, RecvStream, SendStream, VarInt, presets},
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    IrohRelay,
    media::PeerByteResponse,
    network::{
        MAX_FRAME, MediaReader, canonical_address, dns_resolver, read_frame, send_error,
        write_frame,
    },
    relay::opened,
};

const ROOM_PROTOCOL: &[u8] = b"rufin/room/1";
/// A member asks another for audio: one connection, one request.
const MEDIA_PROTOCOL: &[u8] = b"rufin/room-media/1";
/// The most one room message holds: a page of queue entries as text.
const ROOM_FRAME: usize = 256 * 1024;
/// The most a request for audio holds.
const MEDIA_ASK: usize = 4096;
const REACH: Duration = Duration::from_secs(30);
/// How long a room's connection that carries nothing lasts. Both ends keep it alive every
/// few seconds, so a member that went away is noticed in this time, and a member that
/// lost the room looks for the way back.
const SILENCE_MILLIS: u32 = 15_000;
/// How long an answer waits for a path that may carry audio.
const MEDIA_WAIT: Duration = Duration::from_secs(10);

/// A member asks this device for audio. Dropping it leaves the member unanswered.
pub struct MediaAsk {
    pub peer: String,
    pub ask: Value,
    connection: Connection,
    send: SendStream,
    relay: IrohRelay,
}

impl MediaAsk {
    /// Sends the audio over a path that may carry it: a direct one, or the room's own custom
    /// relay. Answers whether such a path was there.
    pub async fn answer(mut self, response: PeerByteResponse) -> Result<bool> {
        let transfer =
            (self.relay).send_media(&self.connection, &mut self.send, response, MEDIA_WAIT);
        tokio::select! {
            biased;
            // The member stopped listening to it, or has heard all of it. That closes the
            // path the audio went over too, which is no failure of the sending.
            _ = self.connection.closed() => Ok(true),
            sent = transfer => sent,
        }
    }

    pub async fn refuse(mut self, reason: &str) {
        let _ = send_error(&mut self.send, reason).await;
    }
}

/// Takes an endpoint's incoming connections: the members of a room it hosts, and members
/// asking it for audio.
fn accept(
    endpoint: Endpoint,
    stop: CancellationToken,
    members: Option<mpsc::Sender<Link>>,
    asks: mpsc::Sender<MediaAsk>,
    relay: IrohRelay,
) {
    tokio::spawn(async move {
        loop {
            let incoming = tokio::select! {
                _ = stop.cancelled() => break,
                incoming = endpoint.accept() => incoming,
            };
            let Some(incoming) = incoming else { break };
            let (members, asks) = (members.clone(), asks.clone());
            let (stop, relay) = (stop.clone(), relay.clone());
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                if connection.alpn() == MEDIA_PROTOCOL {
                    let Ok((send, mut recv)) = connection.accept_bi().await else {
                        return;
                    };
                    let Ok(ask) = read_frame::<Value>(&mut recv, MEDIA_ASK).await else {
                        return;
                    };
                    let peer = connection.remote_id().to_string();
                    let ask = MediaAsk {
                        peer,
                        ask,
                        connection,
                        send,
                        relay,
                    };
                    let _ = asks.send(ask).await;
                    return;
                }
                let Some(members) = members else {
                    return;
                };
                let Ok((send, recv)) = connection.accept_bi().await else {
                    return;
                };
                let _ = members.send(link(connection, send, recv, stop)).await;
            });
        }
    });
}

/// The other end of one member's connection. `frames` ends when the connection does.
pub struct Link {
    pub peer: String,
    pub send: mpsc::Sender<Value>,
    pub frames: mpsc::Receiver<Value>,
}

/// A room's endpoint. Dropping it closes the room's connections.
pub struct Room {
    endpoint: Endpoint,
    stop: CancellationToken,
    relay: IrohRelay,
    /// The members asking this device for audio, until someone takes them to answer.
    asks: Mutex<Option<mpsc::Receiver<MediaAsk>>>,
}

impl Drop for Room {
    fn drop(&mut self) {
        self.stop.cancel();
        let endpoint = self.endpoint.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { endpoint.close().await });
        }
    }
}

impl Room {
    async fn bind(mode: RelayMode) -> Result<Endpoint> {
        let transport = QuicTransportConfig::builder()
            .max_idle_timeout(Some(VarInt::from_u32(SILENCE_MILLIS).into()))
            .build();
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::generate())
            .transport_config(transport)
            .relay_mode(mode)
            .alpns(vec![ROOM_PROTOCOL.to_vec(), MEDIA_PROTOCOL.to_vec()]);
        if let Some(resolver) = dns_resolver() {
            builder = builder.dns_resolver(resolver);
        }
        Ok(builder.bind().await?)
    }

    /// Opens a room on `relay` and yields each member that connects.
    pub async fn host(relay: &IrohRelay) -> Result<(Self, mpsc::Receiver<Link>)> {
        let endpoint = Self::bind(relay.mode()?).await?;
        let stop = CancellationToken::new();
        let (members, joined) = mpsc::channel(16);
        let (asking, asks) = mpsc::channel(16);
        accept(
            endpoint.clone(),
            stop.clone(),
            Some(members),
            asking,
            relay.clone(),
        );
        Ok((
            Self {
                endpoint,
                stop,
                relay: relay.clone(),
                asks: Mutex::new(Some(asks)),
            },
            joined,
        ))
    }

    /// The members asking this device for audio. They are taken once.
    pub fn media_asks(&self) -> Option<mpsc::Receiver<MediaAsk>> {
        self.asks.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    /// Where another member reaches this device for audio: the room's relay to find it, and
    /// its own addresses to send over.
    pub fn reach(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Asks the member at `holder` for audio. What it sends is read while its connection
    /// lasts.
    pub async fn fetch(&self, holder: EndpointAddr, ask: &Value) -> Result<PeerByteResponse> {
        let asking = async {
            let connection = self.endpoint.connect(holder, MEDIA_PROTOCOL).await?;
            let (mut send, mut recv) = connection.open_bi().await?;
            write_frame(&mut send, ask, MEDIA_ASK).await?;
            send.finish()?;
            let about: Result<Value, String> = read_frame(&mut recv, MAX_FRAME).await?;
            anyhow::Ok((connection, about, recv))
        };
        let (connection, about, recv) = tokio::time::timeout(REACH, asking)
            .await
            .context("The member is unreachable")??;
        Ok(PeerByteResponse {
            metadata: about.map_err(anyhow::Error::msg)?,
            reader: Box::pin(MediaReader {
                _connection: connection,
                recv,
            }),
        })
    }

    /// The relay this room is reached through: the host's choice, which a guest's invite
    /// named.
    pub fn relay(&self) -> &IrohRelay {
        &self.relay
    }

    /// Where guests reach this room. With a relay the address names only it, so an invite
    /// does not carry the host's own addresses.
    pub async fn address(&self) -> EndpointAddr {
        if self.relay != IrohRelay::Off {
            let _ = tokio::time::timeout(Duration::from_secs(10), self.endpoint.online()).await;
        }
        let mut address = self.endpoint.addr();
        if address
            .addrs
            .iter()
            .any(|route| matches!(route, TransportAddr::Relay(_)))
        {
            address
                .addrs
                .retain(|route| matches!(route, TransportAddr::Relay(_)));
        }
        address
    }

    /// Reaches the room at `host`, through the relay its address names and `token` opens:
    /// joining a room uses the room's relay, whatever this device's own choice is.
    pub async fn join(host: EndpointAddr, token: Option<String>) -> Result<(Self, Link)> {
        // The relay is dialed under the name it is connected through.
        let host = canonical_address(host);
        let relays = host
            .addrs
            .iter()
            .filter_map(|route| match route {
                TransportAddr::Relay(url) => Some(url.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let relay = (relays.first()).map_or(IrohRelay::Off, |relay| {
            IrohRelay::of(relay, token.as_deref())
        });
        let mode = if relays.is_empty() {
            RelayMode::Disabled
        } else {
            opened(relays, token)
        };
        let endpoint = Self::bind(mode).await?;
        let stop = CancellationToken::new();
        let (asking, asks) = mpsc::channel(16);
        accept(endpoint.clone(), stop.clone(), None, asking, relay.clone());
        let room = Self {
            endpoint,
            stop,
            relay,
            asks: Mutex::new(Some(asks)),
        };
        let link = room.reach_host(host).await?;
        Ok((room, link))
    }

    /// Connects to the room's host, again after the connection to it ended: the member
    /// stays who it was.
    pub async fn reach_host(&self, host: EndpointAddr) -> Result<Link> {
        let connecting = async {
            let connection = (self.endpoint)
                .connect(canonical_address(host), ROOM_PROTOCOL)
                .await?;
            let (send, recv) = connection.open_bi().await?;
            anyhow::Ok(link(connection, send, recv, self.stop.clone()))
        };
        tokio::time::timeout(REACH, connecting)
            .await
            .context("The room is unreachable")?
    }
}

/// Carries frames both ways on one stream until either side ends it.
fn link(
    connection: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    stop: CancellationToken,
) -> Link {
    let peer = connection.remote_id().to_string();
    let (outgoing, mut queued) = mpsc::channel::<Value>(64);
    let (incoming, frames) = mpsc::channel::<Value>(64);
    let writing = connection.clone();
    tokio::spawn(async move {
        while let Some(frame) = queued.recv().await {
            if write_frame(&mut send, &frame, ROOM_FRAME).await.is_err() {
                break;
            }
        }
        writing.close(0u32.into(), b"left");
    });
    tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                _ = stop.cancelled() => break,
                frame = read_frame::<Value>(&mut recv, ROOM_FRAME) => frame,
            };
            let Ok(frame) = frame else { break };
            if incoming.send(frame).await.is_err() {
                break;
            }
        }
        connection.close(0u32.into(), b"left");
    });
    Link {
        peer,
        send: outgoing,
        frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_relay::server::{
        Access, AccessControl, ClientRequest, RelayConfig, Server, ServerConfig,
    };
    use std::{
        net::Ipv4Addr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    /// A relay that admits only who brings its token, and counts them.
    #[derive(Debug)]
    struct Locked(&'static str, AtomicUsize);

    impl AccessControl for Locked {
        async fn on_connect(&self, request: &ClientRequest) -> Access {
            if request.auth_token().as_deref() == Some(self.0) {
                self.1.fetch_add(1, Ordering::Relaxed);
                Access::Allow
            } else {
                Access::Deny { reason: None }
            }
        }
    }

    #[tokio::test]
    async fn a_member_sends_audio_to_another_who_asks() {
        use tokio::io::AsyncReadExt;
        let (room, _members) = Room::host(&IrohRelay::Off).await.unwrap();
        let (guest, _to_host) = Room::join(room.address().await, None).await.unwrap();
        let mut asks = room.media_asks().unwrap();
        let answering = async {
            let ask = asks.recv().await.unwrap();
            assert_eq!(ask.ask, serde_json::json!({"entry": 7}));
            let about = serde_json::json!({"length": 5});
            let response = PeerByteResponse {
                metadata: about,
                reader: Box::pin(&b"audio"[..]),
            };
            assert!(ask.answer(response).await.unwrap());
        };
        let asking = async {
            let ask = serde_json::json!({"entry": 7});
            let mut answer = guest.fetch(room.reach(), &ask).await.unwrap();
            let mut bytes = Vec::new();
            answer.reader.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(answer.metadata, serde_json::json!({"length": 5}));
            assert_eq!(bytes, b"audio");
        };
        tokio::join!(answering, asking);
    }

    #[tokio::test]
    async fn a_locked_relay_admits_the_host_and_the_guest_an_invite_brings() {
        let locked = Arc::new(Locked("friends", AtomicUsize::new(0)));
        let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
        relay.access = locked.clone();
        let mut config = ServerConfig::default();
        config.relay = Some(relay);
        let server = Server::spawn(config).await.unwrap();
        let choice = IrohRelay::Custom {
            url: format!("http://{}/?token=friends", server.http_addr().unwrap()),
        };
        let (room, mut members) = Room::host(&choice).await.unwrap();
        let address = room.address().await;
        // The relay let the host in, so the invite names the relay alone.
        assert!(
            (address.addrs.iter()).all(|route| matches!(route, TransportAddr::Relay(_)))
                && !address.addrs.is_empty()
        );
        assert!(
            !address
                .addrs
                .iter()
                .any(|route| format!("{route:?}").contains("friends"))
        );
        let (guest, to_host) = Room::join(address, choice.token()).await.unwrap();
        assert!(guest.relay().same(&choice));
        to_host
            .send
            .send(serde_json::json!({"hello": 1}))
            .await
            .unwrap();
        let mut member = members.recv().await.unwrap();
        assert_eq!(
            member.frames.recv().await.unwrap(),
            serde_json::json!({"hello": 1})
        );
        assert_eq!(locked.1.load(Ordering::Relaxed), 2);
        drop((guest, room));
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_guest_and_the_host_exchange_frames_and_see_each_other_leave() {
        let (room, mut members) = Room::host(&IrohRelay::Off).await.unwrap();
        let (guest, mut to_host) = Room::join(room.address().await, None).await.unwrap();
        to_host
            .send
            .send(serde_json::json!({"hello": "Robin"}))
            .await
            .unwrap();
        let mut member = members.recv().await.unwrap();
        assert_eq!(
            member.frames.recv().await.unwrap(),
            serde_json::json!({"hello": "Robin"})
        );
        member
            .send
            .send(serde_json::json!({"welcome": 1}))
            .await
            .unwrap();
        assert_eq!(
            to_host.frames.recv().await.unwrap(),
            serde_json::json!({"welcome": 1})
        );
        drop(guest);
        assert!(member.frames.recv().await.is_none());
    }
}
