use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Representação compacta e versionada de eventos, independente das colunas SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEvent {
    /// Delta desde o evento anterior, em milissegundos.
    pub delta_ms: u32,
    pub kind: RawEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawEventKind {
    Insert { text: String, correct: bool },
    Backspace,
    Restart,
    Finish,
    Fail,
}

pub struct RawEventCodec;

impl RawEventCodec {
    pub const VERSION: u16 = 1;

    pub fn encode(events: &[RawEvent]) -> Result<(usize, Vec<u8>)> {
        let postcard = postcard::to_allocvec(events).context("serialize raw events")?;
        let size = postcard.len();
        Ok((size, zstd::stream::encode_all(postcard.as_slice(), 3)?))
    }

    pub fn decode(
        version: u16,
        uncompressed_size: usize,
        compressed: &[u8],
    ) -> Result<Vec<RawEvent>> {
        anyhow::ensure!(
            version == Self::VERSION,
            "unsupported raw event codec version {version}"
        );
        let decoded = zstd::stream::decode_all(compressed)?;
        anyhow::ensure!(
            decoded.len() == uncompressed_size,
            "raw event size does not match its header"
        );
        postcard::from_bytes(&decoded).context("decode raw events")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trips_and_rejects_wrong_version() {
        let events = vec![
            RawEvent {
                delta_ms: 0,
                kind: RawEventKind::Insert {
                    text: "olá".into(),
                    correct: true,
                },
            },
            RawEvent {
                delta_ms: 17,
                kind: RawEventKind::Backspace,
            },
        ];
        let (size, blob) = RawEventCodec::encode(&events).unwrap();
        assert_eq!(RawEventCodec::decode(1, size, &blob).unwrap(), events);
        assert!(RawEventCodec::decode(2, size, &blob).is_err());
    }
}
