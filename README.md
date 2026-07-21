# tuipe

Treinador de digitação adaptativo, offline e nativo de terminal. A interação e
as métricas seguem o Monkeytype; o currículo observa dificuldades recorrentes e
escolhe automaticamente o próximo treino, sem exigir que o usuário entenda ou
configure o modelo.

O código está em **candidato a release 0.1.0**. Motor, persistência, recuperação,
empacotamento e jornada principal são validados automaticamente, inclusive
dentro de um pseudo-terminal real. Os gates ainda abertos para publicação estão
registrados no `PLAN.md` e no registro de mudanças.

## Instalação

### Binário Linux

Baixe o pacote `tuipe-VERSÃO-x86_64-linux.tar.gz` e o arquivo `.sha256` de
mesmo nome no release. Verifique, extraia e instale o binário no diretório do
usuário:

```sh
sha256sum -c tuipe-VERSÃO-x86_64-linux.tar.gz.sha256
tar -xzf tuipe-VERSÃO-x86_64-linux.tar.gz
install -Dm755 tuipe-VERSÃO-x86_64-linux/tuipe-x86_64-linux ~/.local/bin/tuipe
tuipe --version
```

O binário requer Linux x86-64 com glibc 2.29 ou mais recente. `~/.local/bin`
precisa estar no `PATH`.

### Cargo

Depois da publicação no crates.io:

```sh
cargo install tuipe --locked
```

### Desenvolvimento

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

No Kitty, inclusive quando ele hospeda uma sessão tmux, o tuipe detecta o
cliente e usa os ícones Nerd Font. Nos demais terminais, o conjunto Unicode
seguro é usado por padrão, pois não existe um protocolo portátil para descobrir
a fonte ativa. A seleção pode ser sobrescrita sem recompilar:

```sh
TUIPE_ICONS=nerd tuipe
TUIPE_ICONS=unicode tuipe
```

As cores são detectadas automaticamente e degradadas de RGB para 256 ou 16
cores quando necessário. Para diagnosticar uma detecção incorreta, use
`TUIPE_COLORS=truecolor`, `TUIPE_COLORS=256`, `TUIPE_COLORS=16` ou
`TUIPE_COLORS=none`. Um valor explícito também prevalece sobre `NO_COLOR`; sem
`TUIPE_COLORS`, a convenção `NO_COLOR` continua sendo respeitada. Papéis sem
contraste suficiente recebem o menor ajuste necessário durante a renderização;
os arquivos originais dos temas não são alterados.

Temas pessoais podem ser adicionados como TOML em
`$XDG_CONFIG_HOME/tuipe/themes/NOME.toml` (ou
`~/.config/tuipe/themes/NOME.toml`). O arquivo usa os campos `bg`, `main`,
`caret`, `sub`, `subAlt`, `text`, `error`, `errorExtra`, `colorfulError` e
`colorfulErrorExtra`, todos com cores CSS. Um tema inválido é ignorado com um
aviso na interface e não impede o aplicativo de abrir.

## Uso

Basta digitar. Avaliações de progresso, revisões de retenção e testes com
palavras novas são agendados pelo próprio tuipe quando há evidência para isso;
eles não são escolhas adicionais nem ocupam a tela principal.

### Atalhos padrão do teste

| Tecla | Ação |
| --- | --- |
| texto e `espaço` | digitar e confirmar palavras |
| `backspace` | apagar o último caractere |
| `ctrl+w` ou `ctrl+backspace` | apagar a palavra atual |
| `ctrl+c` | cancelar o teste atual e voltar ao início |
| `ctrl+s` | abrir as estatísticas antes de iniciar um teste |
| `enter` | reiniciar ou abrir o próximo teste |
| `esc` | abrir ou fechar as configurações |
| `r` | repetir o mesmo teste após o resultado |
| `s` | abrir as estatísticas após o resultado |
| `f` | favoritar ou desfavoritar a citação após o resultado |
| `q` | sair na tela de resultado ou nas configurações |

Os atalhos `r`, `s`, `f` e `q` ficam bloqueados por 300 ms após o resultado para
evitar uma ação acidental causada pela última tecla do teste.

Ao terminar, a tela identifica sem ambiguidade um teste concluído ou uma falha.
Um WPM superior ao histórico de testes concluídos com a mesma configuração
ativa uma celebração animada de recorde pessoal. A comparação inclui modo e
valor, idioma, vocabulário, pontuação, números, dificuldade e treino; falhas
nunca criam recordes. O primeiro teste concluído de uma configuração estabelece
sua primeira marca.

Em terminais largos, as configurações usam um painel mestre–detalhe: `↑` e `↓`
percorrem a lista alinhada de preferências, enquanto `←`, `→` e `enter` atuam
somente sobre a preferência destacada. Em terminais compactos, a mesma
navegação aparece em uma lista de coluna única.

As setas laterais alteram a opção e `enter` confirma o valor atual e fecha o
painel. Na dificuldade normal, erros podem ser corrigidos sem encerrar o teste;
especialista encerra ao confirmar uma palavra incorreta com espaço; mestre
encerra no primeiro caractere incorreto.

Os atalhos de aplicação podem ser alterados em `config.toml`. O tuipe usa a
notação da crate Crokey e aceita uma tecla com modificadores por ação:

```toml
[keymap]
next = "Enter"
repeat = "Ctrl-r"
statistics = "s"
statistics_global = "Ctrl-s"
favorite = "f"
quit = "q"
settings = "Esc"
cancel = "Ctrl-c"
delete_word = ["Ctrl-w", "Ctrl-Backspace"]
```

A interface sempre mostra as combinações configuradas. Combinações duplicadas,
sequências de várias teclas ou uma lista vazia para `delete_word` invalidam a
configuração; o arquivo é preservado como `config-corrompida-*.toml` e os
atalhos seguros são restaurados.

### Estatísticas e diagnóstico

As estatísticas possuem três páginas próprias. `1`, `2`, `3`, `tab` ou as setas
laterais alternam entre visão geral, progresso e histórico. A visão geral usa
todas as tentativas válidas e suaviza a tendência de WPM ao longo do tempo;
tentativas interrompidas, curtas demais ou muito abaixo da base pessoal não
distorcem o gráfico. Os eixos mostram de três a seis referências conforme o
espaço disponível. O progresso detalha a distribuição de WPM como parcela dos
testes válidos e a atividade diária como minutos, testes e tempo relativo.
Uma barra de comandos separada do conteúdo reúne os controles de cada página.
O histórico pode ser filtrado com `f`; `↑`/`↓` ou `j`/`k` selecionam uma sessão
e `enter` abre seu diagnóstico.

Na visão geral, `↑`/`↓` ou `j`/`k` percorrem as palavras prioritárias e `enter`
abre o diagnóstico da palavra. O detalhe mostra apenas o aumento de chance
de **realmente começar a digitá-la** causado pelo treino adaptativo, descontando
a chance representativa normal. Palavras apenas geradas depois do ponto em que
o usuário costuma terminar não entram nessa conta. A estimativa usa a curva de
alcance reconstruída dos eventos brutos de contextos comparáveis; durações
menores nunca são extrapoladas além do alcance observado. Sem histórico útil, o
teste permanece representativo em vez de presumir uma velocidade.
O detalhe também mostra falhas, correções, ritmo contra a base pessoal,
tendência, recência, padrões relacionados e tentativas recentes. Uma correção
isolada permanece abaixo do limiar de dificuldade acionável. As páginas,
palavras e sessões também podem ser abertas com o mouse.

`r` no detalhe solicita o reset daquela palavra. `R` no panorama solicita o
reset do modelo adaptativo inteiro. Ambos exigem confirmação e preservam
sessões, métricas, eventos brutos, XP e streak.

### Configurações

Na janela aberta por `esc`, a linha em foco é marcada com `›` e o valor ativo
recebe a cor principal. `↑`/`↓` ou `tab` movem o foco, `←` escolhe o valor
anterior, `→` escolhe o próximo e `enter` alterna a opção atual. As alterações
são aplicadas e salvas imediatamente. A legenda permanece visível no rodapé.

Os atalhos diretos continuam disponíveis para quem preferir:

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
Se o SQLite confirmar corrupção estrutural, o banco também é preservado com o
prefixo `tuipe-corrompido-`, um banco novo é criado e a interface explica a
recuperação. Erros de permissão, disco cheio e bancos de versões futuras nunca
são tratados como corrupção nem substituídos automaticamente.

Falhas fatais e panics geram um relatório privado em
`$XDG_STATE_HOME/tuipe/falha-*.log` (ou `~/.local/state/tuipe`). O relatório
contém versão, terminal, causa e backtrace, mas não inclui o texto digitado nem
outras variáveis do ambiente. Ele pode ser anexado ao pedir suporte.

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

As métricas, as observações consultáveis e o modelo adaptativo podem ser
recalculados usando a configuração, a proveniência da seleção, os estímulos e os
eventos brutos guardados em cada sessão:

```sh
tuipe rebuild
```

Sessões antigas que não registravam os estímulos continuam preservadas e são
ignoradas nessa reconstrução. Um bloqueio de escrita impede que outra instância
altere o histórico durante a troca. Nada derivado é substituído antes que todas
as sessões reconstruíveis tenham sido validadas.

Se uma política adaptativa causar um problema operacional, ela pode ser trocada
atomicamente pela distribuição uniforme segura, sem apagar configuração ou
histórico. O mesmo comando restaura a versão anterior depois da investigação:

```sh
tuipe policy status
tuipe policy rollback
```

O aplicativo explica na abertura quando está no modo seguro. Essa operação é um
recurso de recuperação; o usuário não escolhe exercícios nem pesos do modelo.
Enquanto a distribuição uniforme está ativa, a política adaptativa anterior
continua sendo executada em shadow mode com a mesma seed. Estímulos, decisões e
propensões candidatos são persistidos separadamente e nunca alteram o teste
mostrado ao usuário.

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
cargo bench --bench latencia_input_render
scripts/build-portable-linux.sh
```
