use std::fmt::{self, Display};

use libp2p::PeerId;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Eq, Hash, Clone, Debug, PartialEq)]
pub struct ApplicationId(pub String);

impl Display for ApplicationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl From<String> for ApplicationId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl Into<String> for ApplicationId {
    fn into(self) -> String {
        self.0
    }
}

impl AsRef<str> for ApplicationId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(
        serialize_with = "serialize_optional_peer_id",
        deserialize_with = "deserialize_optional_peer_id"
    )]
    pub source: Option<PeerId>,
    pub data: Vec<u8>,
}

impl ChatMessage {
    pub fn new(source: Option<libp2p::PeerId>, data: Vec<u8>) -> Self {
        Self { source, data }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CatchupStreamMessage {
    Request(CatchupRequest),
    Response(CatchupResponse),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatchupRequest {
    pub application_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatchupResponse {
    pub messages: Vec<ChatMessage>,
}

fn serialize_optional_peer_id<S>(peer_id: &Option<PeerId>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match peer_id {
        Some(id) => serializer.serialize_some(&id.to_bytes()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_peer_id<'de, D>(deserializer: D) -> Result<Option<PeerId>, D::Error>
where
    D: Deserializer<'de>,
{
    // Create a visitor that can handle deserializing an optional byte array
    struct PeerIdVisitor;

    impl<'de> Visitor<'de> for PeerIdVisitor {
        type Value = Option<PeerId>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an optional byte array")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            let bytes: Vec<u8> = Deserialize::deserialize(deserializer)?;
            PeerId::from_bytes(&bytes)
                .map(Some)
                .map_err(de::Error::custom)
        }
    }

    // Deserialize the optional byte array using the visitor
    deserializer.deserialize_option(PeerIdVisitor)
}
