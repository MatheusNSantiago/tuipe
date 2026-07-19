use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::typing::{RecordedInputEvent, RecordedInputKind};

/// Representação compacta e versionada de eventos, independente das colunas SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEvent {
    /// Delta desde o evento anterior, em milissegundos.
    pub delta_ms: u32,
    pub kind: RawEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawEventKind {
    Input {
        word_index: u32,
        event: RecordedInputKind,
    },
    Terminal(RawSessionEnd),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawSessionEnd {
    Completed,
    Failed,
    Restarted,
    Quit,
}

pub struct RawEventCodec;

impl RawEventCodec {
    pub const VERSION: u16 = 2;

    /// Converte o relógio monotônico absoluto do motor em deltas compactos e
    /// acrescenta a causa terminal da sessão.
    pub fn materialize(
        events: &[RecordedInputEvent],
        ended_at_ms: u64,
        end: RawSessionEnd,
    ) -> Vec<RawEvent> {
        let mut previous = 0_u64;
        let mut raw = events
            .iter()
            .map(|event| {
                let delta_ms = event
                    .at_ms
                    .saturating_sub(previous)
                    .try_into()
                    .unwrap_or(u32::MAX);
                previous = event.at_ms;
                RawEvent {
                    delta_ms,
                    kind: RawEventKind::Input {
                        word_index: event.word_index.try_into().unwrap_or(u32::MAX),
                        event: event.kind.clone(),
                    },
                }
            })
            .collect::<Vec<_>>();
        raw.push(RawEvent {
            delta_ms: ended_at_ms
                .saturating_sub(previous)
                .try_into()
                .unwrap_or(u32::MAX),
            kind: RawEventKind::Terminal(end),
        });
        raw
    }

    pub fn encode(events: &[RawEvent]) -> Result<(usize, Vec<u8>)> {
        let postcard = postcard::to_allocvec(events).context("serializar eventos brutos")?;
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
            "versão não suportada do codec de eventos brutos: {version}"
        );
        let decoded = zstd::stream::decode_all(compressed)?;
        anyhow::ensure!(
            decoded.len() == uncompressed_size,
            "o tamanho dos eventos brutos não corresponde ao cabeçalho"
        );
        postcard::from_bytes(&decoded).context("decodificar eventos brutos")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_preserva_caminho_de_edicao_e_causa_terminal() {
        let recorded = vec![RecordedInputEvent {
            at_ms: 17,
            word_index: 3,
            kind: RecordedInputKind::Insert {
                grapheme: "á".into(),
                expected: Some("a".into()),
                input_before: String::new(),
                input_after: "á".into(),
                correct: false,
            },
        }];
        let events = RawEventCodec::materialize(&recorded, 40, RawSessionEnd::Failed);
        assert_eq!(events[0].delta_ms, 17);
        assert_eq!(events[1].delta_ms, 23);
        let (size, blob) = RawEventCodec::encode(&events).unwrap();
        assert_eq!(
            RawEventCodec::decode(RawEventCodec::VERSION, size, &blob).unwrap(),
            events
        );
        assert!(RawEventCodec::decode(1, size, &blob).is_err());
    }
}
