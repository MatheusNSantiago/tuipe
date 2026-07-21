use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Read};

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
    pub const VERSION: u16 = 3;
    pub const MAX_UNCOMPRESSED_SIZE: usize = 8 * 1024 * 1024;
    const MAX_EVENTS: usize = 100_000;
    const MAX_EVENT_TEXT: usize = 64 * 1024;

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
        Self::validate(events)?;
        let postcard = postcard::to_allocvec(events).context("serializar eventos brutos")?;
        anyhow::ensure!(
            postcard.len() <= Self::MAX_UNCOMPRESSED_SIZE,
            "eventos brutos excedem o limite de segurança"
        );
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
        anyhow::ensure!(
            uncompressed_size <= Self::MAX_UNCOMPRESSED_SIZE,
            "eventos brutos excedem o limite de segurança"
        );
        let decoder = zstd::stream::read::Decoder::new(compressed)?;
        let mut decoded = Vec::with_capacity(uncompressed_size);
        decoder
            .take((uncompressed_size as u64).saturating_add(1))
            .read_to_end(&mut decoded)?;
        anyhow::ensure!(
            decoded.len() == uncompressed_size,
            "o tamanho dos eventos brutos não corresponde ao cabeçalho"
        );
        let events: Vec<RawEvent> =
            postcard::from_bytes(&decoded).context("decodificar eventos brutos")?;
        Self::validate(&events)?;
        Ok(events)
    }

    /// Repassa o fluxo como um editor determinístico. Isso detecta blobs
    /// íntegros na compressão, mas semanticamente impossíveis.
    pub fn validate(events: &[RawEvent]) -> Result<()> {
        anyhow::ensure!(
            events.len() <= Self::MAX_EVENTS,
            "sessão bruta possui eventos demais"
        );
        let mut inputs = HashMap::<u32, String>::new();
        let mut terminal = false;
        for event in events {
            anyhow::ensure!(!terminal, "há eventos depois do término da sessão");
            match &event.kind {
                RawEventKind::Terminal(_) => terminal = true,
                RawEventKind::Input { word_index, event } => match event {
                    RecordedInputKind::InsertDelta { grapheme, .. } => {
                        anyhow::ensure!(
                            !grapheme.is_empty() && grapheme.len() <= Self::MAX_EVENT_TEXT,
                            "delta de inserção possui tamanho inválido"
                        );
                        inputs.entry(*word_index).or_default().push_str(grapheme);
                    }
                    RecordedInputKind::DeleteDelta {
                        deleted_graphemes, ..
                    } => {
                        let current = inputs.entry(*word_index).or_default();
                        anyhow::ensure!(
                            usize::from(*deleted_graphemes)
                                <= unicode_segmentation::UnicodeSegmentation::graphemes(
                                    current.as_str(),
                                    true
                                )
                                .count(),
                            "delta de exclusão ultrapassa a entrada existente"
                        );
                        remove_last_graphemes(current, *deleted_graphemes);
                    }
                    RecordedInputKind::Focus { .. } | RecordedInputKind::PasteRedacted { .. } => {}
                },
            }
        }
        anyhow::ensure!(terminal, "a sessão bruta não possui causa terminal");
        Ok(())
    }
}

fn remove_last_graphemes(text: &mut String, count: u16) {
    use unicode_segmentation::UnicodeSegmentation;

    for _ in 0..count {
        let Some((index, _)) = text.grapheme_indices(true).next_back() else {
            break;
        };
        text.truncate(index);
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
            kind: RecordedInputKind::InsertDelta {
                grapheme: "á".into(),
                expected: Some("a".into()),
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

    #[test]
    fn decoder_limita_a_descompressao_antes_de_alocar_o_blob_inteiro() {
        let oversized = vec![0_u8; RawEventCodec::MAX_UNCOMPRESSED_SIZE + 1];
        let blob = zstd::stream::encode_all(oversized.as_slice(), 1).unwrap();

        assert!(RawEventCodec::decode(RawEventCodec::VERSION, oversized.len(), &blob).is_err());
    }

    #[test]
    fn codec_rejeita_caminho_de_edicao_impossivel() {
        let events = vec![
            RawEvent {
                delta_ms: 1,
                kind: RawEventKind::Input {
                    word_index: 0,
                    event: RecordedInputKind::DeleteDelta {
                        deleted_graphemes: 1,
                        corrected_graphemes: 0,
                        whole_word: false,
                    },
                },
            },
            RawEvent {
                delta_ms: 0,
                kind: RawEventKind::Terminal(RawSessionEnd::Completed),
            },
        ];
        assert!(RawEventCodec::encode(&events).is_err());
    }
}
