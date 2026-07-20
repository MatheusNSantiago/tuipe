# tuipe

Treinador de digitação adaptativo, offline e nativo de terminal. A interação e
as métricas seguem o Monkeytype; o currículo observa dificuldades recorrentes e
escolhe automaticamente o próximo treino, sem exigir que o usuário entenda ou
configure o modelo.

O projeto ainda está em **alpha** enquanto a experiência e o empacotamento são
fechados. Motor, persistência, recuperação e jornada principal já são validados
automaticamente, inclusive dentro de um pseudo-terminal real.

## Instalação para desenvolvimento

Requisitos:

- Rust 1.88 ou mais recente;
- um terminal UTF-8 com pelo menos 50 colunas por 14 linhas;
- Nerd Font recomendada para os ícones.

```sh
cargo install --path .
tuipe
```

Para executar sem instalar:

```sh
cargo run --release
```

Os ícones usam Nerd Font por padrão. Em terminais sem Nerd Font, o fallback
Unicode pode ser ativado sem recompilar:

```sh
TUIPE_ICONS=unicode tuipe
```

As cores são detectadas automaticamente e degradadas de RGB para 256 ou 16
cores quando necessário. Para diagnosticar uma detecção incorreta, use
`TUIPE_COLORS=truecolor`, `TUIPE_COLORS=256`, `TUIPE_COLORS=16` ou
`TUIPE_COLORS=none`. Papéis sem contraste suficiente recebem o menor ajuste
necessário durante a renderização; os arquivos originais dos temas não são
alterados.

Temas pessoais podem ser adicionados como TOML em
`$XDG_CONFIG_HOME/tuipe/themes/NOME.toml` (ou
`~/.config/tuipe/themes/NOME.toml`). O arquivo usa os campos `bg`, `main`,
`caret`, `sub`, `subAlt`, `text`, `error`, `errorExtra`, `colorfulError` e
`colorfulErrorExtra`, todos com cores CSS. Um tema inválido é ignorado com um
aviso na interface e não impede o aplicativo de abrir.

## Uso

Basta começar a digitar. Avaliações de progresso, revisões de retenção e testes
com palavras novas são agendados pelo próprio tuipe quando há evidência para
isso. Esses contextos aparecem na interface apenas para explicar o teste atual;
eles não são escolhas adicionais.

### Atalhos do teste

| Tecla | Ação |
| --- | --- |
| texto e `espaço` | digitar e confirmar palavras |
| `backspace` | apagar o último caractere |
| `ctrl+w` ou `ctrl+backspace` | apagar a palavra atual |
| `ctrl+c` | cancelar o teste atual e voltar ao início |
| `enter` | reiniciar ou abrir o próximo teste |
| `esc` | abrir ou fechar as configurações |
| `r` | repetir o mesmo teste após o resultado |
| `s` | abrir as estatísticas após o resultado |
| `q` | sair na tela de resultado ou nas configurações |

Os atalhos `r`, `s` e `q` ficam bloqueados por 300 ms após o resultado para
evitar uma ação acidental causada pela última tecla do teste.

### Estatísticas e diagnóstico

Na tela de estatísticas, `↑`/`↓` ou `j`/`k` percorrem as palavras prioritárias e
`enter` abre o diagnóstico da palavra. O detalhe mostra a chance estimada no
próximo treino adaptativo, falhas, correções, ritmo contra a base pessoal,
tendência, recência, sequências relacionadas e tentativas recentes. Também é
possível clicar diretamente numa palavra.

`r` no detalhe solicita o reset daquela palavra. `R` no panorama solicita o
reset do modelo adaptativo inteiro. Ambos exigem confirmação e preservam
sessões, métricas, eventos brutos, XP e streak.

### Configurações

Na janela aberta por `esc`, cada tecla percorre as opções do respectivo grupo:

| Tecla | Configuração |
| --- | --- |
| `m` | modo: tempo, palavras ou citação |
| `v` | duração, quantidade de palavras ou tamanho da citação |
| `d` | dificuldade: normal, especialista ou mestre |
| `p` / `n` | pontuação / números |
| `a` | currículo adaptativo |
| `l` / `k` | idioma / pacote de palavras |
| `q` | sair do tuipe |
| `t` | tema |

## Dados e privacidade

O tuipe funciona integralmente offline e não envia telemetria. No Linux, os
arquivos respeitam as variáveis XDG:

- configuração: `$XDG_CONFIG_HOME/tuipe/config.toml` ou
  `~/.config/tuipe/config.toml`;
- histórico e modelo: `$XDG_DATA_HOME/tuipe/tuipe.db` ou
  `~/.local/share/tuipe/tuipe.db`.

O banco guarda sessões, eventos compactados e projeções do modelo adaptativo.
Uma configuração inválida é isolada com o prefixo `config-corrompida-` e o
aplicativo volta aos padrões sem destruir o arquivo problemático.

Para validar a configuração, a estrutura do banco, a integridade do SQLite e
todos os eventos compactados sem alterar dados:

```sh
tuipe doctor
```

Para produzir uma cópia consistente e privada do banco, inclusive enquanto ele
usa WAL:

```sh
tuipe backup
tuipe backup caminho/para/copia.db
```

Sem um destino explícito, o arquivo recebe data e hora no nome. O comando não
sobrescreve uma cópia existente.

## Desenvolvimento

A referência comportamental congelada é o commit
`781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5` do Monkeytype. `PLAN.md` define o
contrato do produto; `docs/modelo-de-treinamento.md` descreve o modelo adaptativo
e suas limitações. Proveniência e licenças dos assets estão em `NOTICE` e
`assets/manifest.json`.

Validação local completa:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```
