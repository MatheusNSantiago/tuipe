use unicode_segmentation::UnicodeSegmentation;

/// Separador que conclui uma palavra no modelo de entrada original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitCharacter {
    Space,
    Newline,
    None,
}

impl CommitCharacter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Space => " ",
            Self::Newline => "\n",
            Self::None => "",
        }
    }
}

/// A palavra-alvo é armazenada separadamente do caractere de confirmação, como
/// em `test-words.ts` do Monkeytype. Comparações sempre usam `with_commit()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetWord {
    pub text: String,
    pub commit: CommitCharacter,
}

impl TargetWord {
    pub fn from_generated(word: impl Into<String>) -> Self {
        let mut word = word.into();
        let commit = if word.ends_with(' ') {
            word.pop();
            CommitCharacter::Space
        } else if word.ends_with('\n') {
            word.pop();
            CommitCharacter::Newline
        } else {
            CommitCharacter::None
        };

        Self { text: word, commit }
    }

    pub fn with_commit(&self) -> String {
        format!("{}{}", self.text, self.commit.as_str())
    }

    pub fn graphemes_with_commit(&self) -> Vec<String> {
        self.with_commit()
            .graphemes(true)
            .map(str::to_owned)
            .collect()
    }
}

/// Histórico de entrada de uma palavra, incluindo o separador usado para deixá-la.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WordAttempt {
    pub input: String,
    pub committed: bool,
    pub first_keypress_ms: Option<u64>,
    pub last_keypress_ms: Option<u64>,
    pub corrections: u32,
    /// Tempo bruto entre teclas da palavra. A projeção persistente separa
    /// execução e interrupções pela distribuição da sessão.
    pub active_ms: u64,
}

impl WordAttempt {
    pub fn pop_grapheme(&mut self) -> bool {
        let Some((index, _)) = self.input.grapheme_indices(true).next_back() else {
            return false;
        };
        self.input.truncate(index);
        true
    }

    pub fn without_commit(&self) -> String {
        self.input
            .strip_suffix(' ')
            .or_else(|| self.input.strip_suffix('\n'))
            .unwrap_or(&self.input)
            .into()
    }
}
