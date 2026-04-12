use libp2p::{
    gossipsub, mdns, noise,
    swarm::SwarmEvent,
    tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use libp2p::swarm::NetworkBehaviour;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    NewBlock(Vec<u8>),
    NewTransaction(Vec<u8>),
    RequestBlocks { from_height: u64 },
    BlockResponse(Vec<Vec<u8>>),
}

#[derive(NetworkBehaviour)]
pub struct CursBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

pub struct NetworkNode {
    pub peer_id: PeerId,
    pub swarm: Swarm<CursBehaviour>,
    pub topic: gossipsub::IdentTopic,
}

impl NetworkNode {
    pub async fn new(port: u16) -> Result<Self, Box<dyn std::error::Error>> {
        let topic = gossipsub::IdentTopic::new("curs3d-mainnet");

        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| {
                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .heartbeat_interval(Duration::from_secs(10))
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .build()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                )?;

                let mdns = mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?;

                Ok(CursBehaviour { gossipsub, mdns })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", port).parse()?;
        swarm.listen_on(listen_addr)?;

        let peer_id = *swarm.local_peer_id();
        info!("Node started with PeerId: {}", peer_id);

        Ok(NetworkNode {
            peer_id,
            swarm,
            topic,
        })
    }

    pub fn broadcast(&mut self, message: &NetworkMessage) -> Result<(), Box<dyn std::error::Error>> {
        let data = serde_json::to_vec(message)?;
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), data)?;
        Ok(())
    }

    pub async fn run(
        &mut self,
        mut rx: mpsc::Receiver<NetworkMessage>,
        tx: mpsc::Sender<NetworkMessage>,
    ) {
        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    if let Err(e) = self.broadcast(&msg) {
                        warn!("Failed to broadcast: {}", e);
                    }
                }
                event = self.swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message { message, .. }
                        )) => {
                            if let Ok(net_msg) = serde_json::from_slice::<NetworkMessage>(&message.data) {
                                let _ = tx.send(net_msg).await;
                            }
                        }
                        SwarmEvent::Behaviour(CursBehaviourEvent::Mdns(
                            mdns::Event::Discovered(peers)
                        )) => {
                            for (peer_id, _addr) in peers {
                                info!("Discovered peer: {}", peer_id);
                                self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                            }
                        }
                        SwarmEvent::Behaviour(CursBehaviourEvent::Mdns(
                            mdns::Event::Expired(peers)
                        )) => {
                            for (peer_id, _addr) in peers {
                                info!("Peer expired: {}", peer_id);
                                self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                            }
                        }
                        SwarmEvent::NewListenAddr { address, .. } => {
                            info!("Listening on {}", address);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
