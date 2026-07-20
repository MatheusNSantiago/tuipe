# Plano de implementacao — tuipe

## 1. Objetivo

Construir o `tuipe`, um clone local e terminal-based do Monkeytype, escrito em Rust com Ratatui.

A motivacao central e criar um treinador que aprenda, ao longo de todo o historico, quais palavras o usuario erra ou digita com dificuldade e aumente sua frequencia de forma quase imperceptivel. Ele nao pode virar uma lista repetitiva de erros, confundir AFK com dificuldade ou continuar favorecendo uma palavra depois que o desempenho voltou ao normal.

**Regra principal do projeto:** copiar o maximo possivel da experiencia do Monkeytype. O objetivo nao e apenas oferecer um teste de digitacao parecido; e reproduzir o *feel*: disposicao visual, ritmo da interface, inicio e termino do teste, feedback por caractere, quebra e rolagem de linhas, correcoes, atalhos, resultados e ausencia de distracoes.

Quando este plano nao determinar um comportamento explicitamente, consultar o Monkeytype e copiar seu comportamento. Divergencias devem existir somente quando:

1. foram decididas neste plano;
2. a limitacao do terminal torna a copia impossivel; ou
3. ha evidencia mensuravel de que a copia prejudica latencia ou confiabilidade.

Toda divergencia nova deve ser documentada antes de ser implementada.

O projeto sera criado em `/home/matheus/dev/side-projects/tuipe`. Este documento e apenas o plano; a implementacao ainda nao foi iniciada.

### Referencia congelada

Usar como referencia funcional o Monkeytype no commit [`781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5`](https://github.com/monkeytypegame/monkeytype/tree/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5). Fixar o commit evita que uma mudanca posterior do site altere silenciosamente o alvo.

Principais pontos de leitura:

- [configuracao padrao](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/constants/default-config.ts);
- [seletor de modo, tempo e palavras](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/components/pages/test/TestConfig.tsx);
- [geracao de palavras, pontuacao e numeros](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/test/words-generator.ts);
- [selecao uniforme de palavras](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/test/wordset.ts);
- [entrada e edicao](https://github.com/monkeytypegame/monkeytype/tree/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/input);
- [logica do teste](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/test/test-logic.ts) e [UI do teste](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/test/test-ui.ts);
- [calculo das estatisticas](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/test/events/stats.ts);
- [estrutura da tela de resultado](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/html/pages/test-result.html);
- [estatisticas da conta](https://github.com/monkeytypegame/monkeytype/tree/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/components/pages/account);
- [calculo de XP](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/backend/src/api/controllers/result.ts#L691) e [curva de niveis](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/utils/levels.ts);
- [paletas de temas](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/constants/themes.ts);
- [licenca GPL-3.0](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/LICENSE).

## 2. Decisoes fechadas

### Produto e plataforma

- Aplicativo local, offline e para um unico usuario.
- Sem servidor, login, conta ou sincronizacao.
- Linux e a plataforma oficialmente suportada na primeira entrega.
- O codigo nao deve depender desnecessariamente de X11, Wayland, Hyprland ou de um emulador especifico.
- Nome do diretorio, crate e binario: `tuipe`.
- Licenca: GPL-3.0, com atribuicoes do Monkeytype e dos dados reutilizados.
- Abrir diretamente na tela de teste, sem home intermediaria.
- A primeira execucao usa `portuguese/common`, `time 30`, `expert`, `adaptive on`, punctuation/numbers off e tema `arch`.
- Toda mudanca de preferencia deve ser lembrada imediatamente. A proxima execucao restaura o ultimo estado, nao os defaults iniciais.

### Modos e conteudo incluidos

- `time`: 15, 30, 60 e 120 segundos.
- `words`: 10, 25, 50 e 100 palavras.
- `quote`: todos, curtos, medios e longos.
- Dificuldade: `normal`, `expert` e `master`, com semantica identica ao Monkeytype.
- Modificadores: `punctuation`, `numbers` e o novo `adaptive`.
- Idiomas: portugues e ingles.
- Pacotes por idioma: `common`, `1k` e `5k`.
- Compartilhar conhecimento adaptativo da mesma palavra entre pacotes do mesmo idioma.
- Corpus completo local do commit de referencia: 109 citacoes portuguesas e 6.488 inglesas.
- Favoritar citacoes localmente.
- Repetir exatamente o mesmo teste a partir da tela de resultado.

### Temas e controles

Embarcar estas dez paletas: `arch`, `serika_dark`, `serika`, `catppuccin`, `dracula`, `nord`, `gruvbox_dark`, `rose_pine`, `solarized_dark` e `monokai`.

- `arch` e o tema inicial.
- Permitir temas adicionais por arquivo declarativo, sem recompilar.
- Sem editor visual de tema.
- Suportar mouse.
- Permitir atalhos configuraveis.
- Nao reservar letras sem modificador enquanto o teste estiver ativo; toda letra digitavel pertence ao teste.
- Manter dicas discretas de teclas fora da area principal.

### Recursos explicitamente excluidos

- Backend, contas, amizades, rankings, anuncios, notificacoes sociais e sincronizacao.
- Modos `zen` e `custom`.
- Todos os `funboxes`.
- Tags, presets, exportacao CSV e importacao/exportacao de configuracoes.
- Sons, efeitos de particula, efeitos de letras e rolagem animada. O caret padrao permanece fino e piscante, preservando a fonte nativa do terminal.
- Multiplos estilos de cursor; usar um unico cursor coerente com o tema.
- WPM, precisao e burst ao vivo. Eles aparecem apenas depois do teste.
- Teclado virtual, pace caret, blind mode, stop-on-error, confidence mode e freedom mode.
- Limites de WPM/precisao/burst, quick end e outras dificuldades de nicho.
- Emulacao de layout, opposite shift, lazy mode, ingles britanico e code-unindent.
- Tape mode, show-all-lines, unidades alternativas a WPM e casas decimais configuraveis.
- Temas aleatorios/automaticos, imagens de fundo, flip colors e colorful mode.
- Duracoes customizadas, quantidades customizadas e testes infinitos.
- Paleta de comandos e busca nas configuracoes.
- Replay de teclas, screenshot, compartilhamento, copiar resultado e busca manual de citacoes.
- Treino apenas das palavras erradas no ultimo teste; o adaptativo historico o substitui.
- Mascote, badges, achievements, desafios e perfil local. Manter apenas XP e streak.

## 3. Contrato da experiencia

### Tela de teste

Replicar visualmente a tela principal do Monkeytype dentro das limitacoes de uma grade terminal:

1. barra de configuracao compacta e centralizada no topo;
2. area de texto centralizada verticalmente;
3. no maximo tres linhas visiveis, com reflow responsivo e rolagem por linha;
4. palavras futuras em cor secundaria discreta;
5. texto correto, erro, letra extra e cursor usando papeis distintos do tema;
6. linha discreta `português · especialista` acima do texto, sem expor detalhes
   internos do adaptativo;
7. progresso `mini`, sem estatisticas ao vivo;
8. chrome reduzido durante digitacao para preservar foco;
9. dicas de teclas no rodape quando o teste esta ocioso.

Avaliação de progresso, palavras novas, revisão de retenção e repetição podem ser
identificadas discretamente para explicar o contexto atual. Elas nunca se tornam
uma escolha adicional nem revelam os pesos internos do algoritmo.

### Entrada

- Entrar em raw mode e iniciar o cronometro na primeira tecla textual valida, como o Monkeytype.
- Copiar a semantica de insercao, Backspace, letras extras, Space, palavra anterior e termino dos handlers do Monkeytype; nao reinterpretar por intuicao.
- Preservar a regra do Monkeytype que evita repetir uma das duas palavras imediatamente anteriores.
- `Enter` e o quick restart inicial, conforme a configuracao usada como referencia, mas permanece remapeavel.
- Tratar caracteres por grapheme cluster e medir largura de exibicao corretamente. Usar [Unicode Text Segmentation, UAX #29](https://unicode.org/reports/tr29/) e `unicode-width`.
- Reflow em resize nao perde entrada, nao muda a palavra ativa e nao reinicia o cronometro.
- Restaurar raw mode, mouse capture e alternate screen mesmo em panic ou erro.

### Resultado

Copiar as formulas e a hierarquia do resultado do Monkeytype:

- WPM, raw WPM, accuracy, consistency, caracteres corretos/incorretos/extras/perdidos e duracao;
- WPM ao longo do tempo com erros marcados;
- tipo do teste e modificadores;
- acoes: proximo teste, repetir mesmo teste e abrir estatisticas.

Tentativas que falham automaticamente sao persistidas e alimentam o adaptativo, mas nao entram em recordes nem medias de testes concluidos. Restart, saida ou troca manual de configuracao nao atualizam o modelo adaptativo.

## 4. Arquitetura proposta

Usar um unico crate inicialmente. Separar dominios por modulos, nao por microcrates prematuros.

```text
src/
  main.rs                 bootstrap, terminal e lifecycle
  app/                    estado de tela, actions e update loop
  typing/                 motor puro de digitacao e metricas
  content/                word packs, quotes, punctuation e numbers
  adaptive/               observacoes, modelo, policy e sampler
  persistence/            SQLite, migrations e codec de eventos
  ui/                     telas, widgets, layout e temas
  gamification/           XP, levels e streak
assets/
  languages/
  quotes/
  themes/
migrations/
tests/
```

### Fronteiras

- `TestEngine`: reducer puro. Recebe `InputEvent`, produz novo estado e efeitos de dominio. Nao conhece Ratatui ou SQLite.
- `App`: state machine das telas e coordenacao de efeitos. Seguir um fluxo `Model -> Message -> Update -> View`, semelhante a [The Elm Architecture recomendada pelo Ratatui](https://ratatui.rs/concepts/application-patterns/the-elm-architecture/).
- `ContentCatalog`: carrega `WordPack`, `QuotePack` e `Theme` declarativos. Adicionar dados nao altera o motor.
- `WordSampler`: implementacoes `UniformSampler` e `AdaptiveSampler`; ambos recebem RNG injetavel.
- `AdaptivePolicy`: unica estrutura no codigo com todos os parametros da heuristica. Nao e configuracao do usuario.
- `Repository`: operacoes estreitas de persistencia e transacoes; nenhum SQL dentro da UI.
- `RawEventCodec`: formato binario versionado, independente do schema SQL.

Evitar interfaces artificiais. Idiomas, citacoes e temas devem ser extensiveis principalmente por schema de dados. Traits ficam restritas a fronteiras com implementacoes realmente alternativas.

### Stack inicial

- Rust stable compativel com Ratatui 0.30.2 (MSRV 1.88 na revisao consultada).
- [Ratatui](https://ratatui.rs/) para layout/widgets e `TestBackend`.
- [Crossterm events](https://docs.rs/crossterm/0.29.0/crossterm/event/index.html) para teclado, mouse, paste e resize.
- [rusqlite](https://docs.rs/rusqlite/) com SQLite embarcado.
- `serde` + `toml` para configuracao e assets declarativos.
- [Postcard](https://docs.rs/postcard/latest/postcard/) + [zstd](https://docs.rs/zstd/) para eventos brutos compactos.
- `unicode-segmentation`, `unicode-normalization` e `unicode-width`.
- `rand` com RNG seedavel.
- `proptest` e snapshots `insta` para testes.

Fixar versoes no `Cargo.lock` ao iniciar. Nao adicionar runtime async sem necessidade demonstrada.

## 5. Persistencia

Seguir a [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/):

- configuracao: `$XDG_CONFIG_HOME/tuipe/config.toml`, fallback `~/.config/tuipe/config.toml`;
- banco: `$XDG_DATA_HOME/tuipe/tuipe.db`, fallback `~/.local/share/tuipe/tuipe.db`;
- logs descartaveis: `$XDG_STATE_HOME/tuipe/`.

Gravar `config.toml` de forma atomica (`temp file -> fsync -> rename`). Migracoes SQLite sao incrementais e transacionais.

### Schema minimo

- `schema_version`;
- `sessions`: configuracao congelada do teste, estado terminal, metricas e versoes dos algoritmos;
- `word_observations`: palavra, ordem, resultado, correcoes, tempo ativo e pausas ignoradas;
- `word_skill`: estado materializado por idioma + forma lexical;
- `ngram_skill`: sinal secundario de sequencias internas;
- `mechanic_skill`: pontuacao e capitalizacao;
- `favorite_quotes`;
- `xp_state` e `streak_state`;
- `raw_events`: codec version, tamanho original e blob comprimido.

Manter dados derivados reconstruiveis. Cada sessao registra `metrics_version`, `adaptive_version` e `codec_version`.

### Eventos compactos

Nao armazenar JSON por tecla. Para cada sessao:

1. converter timestamps em deltas de milissegundos;
2. serializar enums pequenos, graphemes e flags com Postcard/varints;
3. comprimir o bloco com Zstandard;
4. persistir o blob uma vez ao finalizar, falhar, reiniciar ou sair;
5. manter observacoes por palavra em colunas consultaveis.

Nao impor limite de retencao. Incluir ferramenta interna de verificacao e migracao do codec. O loop de digitacao apenas acumula eventos em memoria; persistencia ocorre fora do caminho critico.

Usar SQLite em WAL mode e transacoes curtas. A documentacao oficial explica concorrencia e checkpointing: [SQLite WAL](https://www.sqlite.org/wal.html). Nao permitir que checkpoint ou recomputacao adaptativa bloqueiem input/render.

## 6. Modelo adaptativo

> A pesquisa e a especificação de implementação deste modelo estão em
> [`docs/modelo-de-treinamento.md`](docs/modelo-de-treinamento.md). O documento
> substitui a estratégia heurística inicial abaixo quando houver divergência,
> especialmente contagens absolutas, limiar fixo de AFK e avaliação no próprio
> material adaptativo.

### Comportamento externo fechado

- `adaptive` e um modificador de `time` e `words`; nao funciona em `quote`.
- Vem ligado na primeira execucao.
- Testes com `adaptive off` e testes `quote` ainda geram evidencias.
- Em `quote`, a evidencia atualiza palavras conhecidas, mas nunca influencia a escolha da citacao.
- Cold start e exatamente uniforme, igual ao Monkeytype.
- A selecao permanece suave: a maioria absoluta das palavras vem da distribuicao comum.
- Alvo inicial: aproximadamente 90% de chance de ao menos uma selecao adaptativa numa sessao, desde que haja evidencia suficiente.
- Uma palavra individual muito problematica raramente deve superar 40% de chance total de aparecer na sessao. Se a chance uniforme natural ja for maior em um teste muito longo, o adaptativo nao a reduz; ele apenas nao aumenta alem do baseline.
- Nao ha garantia, fila, quota rigida ou cooldown.
- Nao existe estado binario de palavra "dominada"; habilidade e dificuldade sao estimativas continuas.
- Nao selecionar uma mesma palavra por boost mais vezes que o peso naturalmente produzir; a propria probabilidade controla repeticao.
- Usar uma curva S/sigmoide saturante. Pouca evidencia quase nao altera a chance; evidencia recorrente acelera o aumento; casos extremos aproximam-se do teto sem explodir.
- Todos os parametros ficam como constantes nomeadas dentro de `AdaptivePolicy` e sao cobertos por testes.

### Evidencias

Ordem de forca:

1. palavra confirmada incorreta: sinal forte;
2. erro corrigido antes de confirmar: sinal moderado;
3. lentidao ativa anormal: sinal leve;
4. palavra correta e rapida: evidencia positiva menor que a penalidade de erro.

Regras:

- No `expert`, preservar a palavra que causou a falha.
- No `master`, preservar a tecla/palavra que causou a falha.
- Um acerto nao cancela um erro; recuperacao exige varios bons resultados em sessoes distintas.
- Repetir exatamente o mesmo teste gera evidencia com peso geometricamente decrescente.
- Evidencia antiga perde influencia suavemente. Depois de boa proficiencia e muito tempo sem exposicao, aplicar apenas um pequeno aumento de exploracao, nunca restaurar a dificuldade antiga sem novo sinal.
- Tempo anterior a primeira tecla da palavra nao conta.
- Medir tempo ativo entre primeira tecla e commit da palavra, incluindo correcoes.
- Classificar gaps excepcionalmente longos como interrupcao/AFK e remove-los, em vez de limita-los artificialmente.
- Comparar tempo por grapheme ao baseline pessoal de palavras com tamanho semelhante. Quando houver amostras suficientes, combinar com o historico da propria palavra.
- Compartilhar a habilidade lexical entre `common`, `1k` e `5k` do mesmo idioma.
- Acentos pertencem a identidade lexical (`esta` != `está`; preservar NFC). Case e pontuacao nao pertencem.
- Pontuacao e capitalizacao mantem sinais separados. Quando `punctuation` estiver ativo, podem inclinar levemente os padroes de formatacao sem penalizar a palavra-base.
- `numbers` mantem o comportamento do Monkeytype: cerca de 10% dos tokens viram numeros aleatorios de 1–4 digitos. Numeros nao alimentam o adaptativo.

### Estrategia inicial recomendada

Implementar o modelo em tres camadas, mantendo os coeficientes ajustaveis no codigo:

1. **Erro/correcao:** contagens fracionarias com prior conservador e decaimento temporal. Erro confirmado pesa mais que correcao; sucesso pesa menos que erro.
2. **Latencia:** residuo robusto do tempo ativo por grapheme contra mediana/MAD pessoal por faixa de tamanho. Saturar outliers restantes.
3. **Generalizacao:** pequeno sinal de bigramas/trigramas internos, ativado somente apos evidencia em palavras diferentes. Nao modelar pares entre palavras.

Combinar os sinais num `difficulty_score`, multiplicar por confianca/amostragem e passar pela sigmoide. Converter o boost em peso de sorteio sem mudar a massa uniforme mais que o necessario.

Para calibrar chance por sessao:

- `words`: o numero de draws e conhecido;
- `time`: estimar draws pela mediana pessoal recente, com fallback conservador no cold start;
- validar a chance real por simulacao, pois a regra de nao repetir as duas palavras anteriores quebra a independencia perfeita dos draws.

Nao esconder heuristica ruim com regras especiais. Ajustar pesos e curva com simulacao e dados reais.

### Transparencia

Na tela principal de estatisticas, mostrar somente uma pequena lista de palavras prioritarias, tendencia e indicador visual discreto. Drill-down por palavra mostra progressivamente:

- chance estimada de aparecer numa sessao adaptativa;
- erros, correcoes e acertos;
- velocidade ativa contra baseline;
- quantidade e recencia das amostras;
- tendencia;
- sequencias internas relevantes.

Permitir reset de uma palavra ou do modelo inteiro, sempre com confirmacao. Nao permitir editar pesos manualmente.

## 7. XP, niveis e streak

Copiar a formula do Monkeytype, removendo somente bonus de `funbox`:

- XP base = `round(segundos_ativos * 2)`;
- +50% para 100% accuracy;
- senao, +25% quando tudo foi corrigido;
- +50% em `quote`;
- +40% com punctuation;
- +10% com numbers;
- streak adiciona linearmente ate +200% em 100 dias;
- aplicar multiplicador de accuracy `(accuracy - 50) / 50` conforme a referencia;
- tentativas incompletas recebem o mesmo credito parcial da referencia: `round(segundos * max((accuracy - 50) / 50, 0))`; portar quando e como esse credito e consolidado, em vez de inventar uma regra local;
- bonus do primeiro teste de um novo dia = 5% do XP total, limitado a 100–1.000;
- gain multiplier = 1.

Usar a mesma curva de niveis de [`levels.ts`](https://github.com/monkeytypegame/monkeytype/blob/781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5/frontend/src/ts/utils/levels.ts). A [configuracao publica atual](https://api.monkeytype.com/configuration) confirma os parametros de streak e XP.

Streak:

- primeiro teste concluido cria streak 1;
- mais testes no mesmo dia nao incrementam;
- um teste no dia seguinte incrementa;
- perder um dia faz o proximo teste recomecar em 1;
- usar meia-noite no fuso local;
- armazenar maior streak historico.

## 8. Plano de execucao

### Fase 0 — bootstrap e proveniencia

- [x] Criar `/home/matheus/dev/side-projects/tuipe` como repositorio Git e crate binario Rust.
- [x] Adicionar GPL-3.0, `NOTICE` e referencias ao commit do Monkeytype.
- [x] Fixar toolchain/dependencias e CI para format, clippy, test e build release.
- [x] Criar script deterministico que importa somente word packs, quotes e dez temas aprovados do commit congelado.
- [x] Verificar e registrar proveniencia/licenciamento dos assets importados.

**Pronto quando:** build vazio funciona em Linux, licencas estao presentes e assets podem ser regenerados bit a bit.

### Fase 1 — motor de digitacao fiel

- [x] Modelar `TestConfig`, `TestState`, `WordState`, `InputEvent` e estados terminais.
- [x] Portar comportamento de input e validacao do Monkeytype para reducer puro.
- [x] Implementar `time`, `words`, `quote`, `normal`, `expert`, `master`, punctuation e numbers.
- [x] Portar formulas de WPM, raw, accuracy, consistency e char stats.
- [x] Implementar RNG seedavel, next test e repeat-same-test.
- [x] Criar testes de mesa comparando sequencias de teclas com o Monkeytype.

**Pronto quando:** os mesmos prompts e eventos produzem os mesmos estados e metricas observaveis da referencia.

### Fase 2 — TUI e feel

- [x] Implementar lifecycle seguro de terminal com Ratatui/Crossterm.
- [x] Construir tela de teste de tres linhas, reflow, rolagem, cursor e papeis de cor.
- [x] Implementar seletores, mini progress, foco limpo e contexto das sessoes automaticas.
- [x] Implementar mouse, resize e keymap configuravel.
- [x] Construir tela de resultado e grafico terminal.
- [x] Criar snapshots em tamanhos pequeno, medio e ultrawide para cada estado importante.

**Pronto quando:** digitacao e visual parecem uma traducao direta do Monkeytype para terminal, sem flicker nem layout jumping.

### Fase 3 — conteudo, temas e configuracao

- [x] Integrar pacotes `common`, `1k`, `5k` para portugues/ingles.
- [x] Integrar corpus de quotes e favoritos.
- [x] Integrar dez temas e schema de tema pessoal.
- [x] Persistir/restaurar configuracao XDG atomicamente.
- [x] Validar assets na inicializacao com mensagens de erro acionaveis.

**Pronto quando:** adicionar um word pack ou tema e uma mudanca de dados, nao do motor.

### Fase 4 — persistencia e historico

- [x] Criar migrations e repositories SQLite.
- [x] Implementar WAL, transacoes curtas e worker de persistencia.
- [x] Implementar codec Postcard + Zstandard versionado.
- [x] Persistir sessoes concluidas, falhas e restarts com classificacao correta.
- [x] Implementar rebuild de metricas/materialized skills a partir dos eventos.
- [x] Testar recovery de crash, migracao e blob corrompido.

**Pronto quando:** nenhuma escrita entra no caminho critico e todo estado derivado pode ser reconstruido.

### Fase 5 — adaptativo

- [x] Implementar extracao de observacoes e detector robusto de AFK.
- [x] Implementar baselines pessoais por idioma/tamanho.
- [x] Implementar word skill, n-gram skill e mechanics separados.
- [x] Implementar `AdaptivePolicy`, sigmoide e sampler misto.
- [x] Implementar compartilhamento entre packs e aprendizagem passiva de quote.
- [ ] Implementar decaimento, recuperacao entre sessoes e desconto de repeated test.
- [x] Implementar reset por palavra/modelo e versionamento/rebuild.
- [x] Criar simulador deterministico de milhares de sessoes para calibracao.

**Pronto quando:** cold start continua uniforme; o boost e incremental; ha ~90% de chance de alguma revisao com evidencia suficiente; nenhuma palavra e levada acima do teto adaptativo; AFK nao altera dificuldade.

### Fase 6 — stats, XP e streak

- [x] Implementar overview e diagnostico acionavel de palavras e padroes.
- [x] Implementar historico filtravel e detalhe de sessao.
- [x] Implementar graficos de evolucao, distribuicao e atividade diaria.
- [x] Implementar lista minimalista de palavras e drill-down.
- [x] Portar XP, levels, daily bonus e streak.
- [x] Excluir falhas de averages/PBs sem perder sua evidencia adaptativa.

**Pronto quando:** todas as informacoes decididas sao acessiveis sem poluir a tela principal.

### Fase 7 — validacao final e entrega

- [x] Fazer comparacao lado a lado com Monkeytype em `time 30`, `words 50` e quotes.
- [x] Medir p50/p95/p99 de input -> state -> render em release.
- [x] Corrigir qualquer hitch de SQLite, recomputacao ou resize.
- [ ] Testar terminais com true color e fallback 256 cores.
- [ ] Testar UTF-8/acentos, terminais pequenos, mouse e keybindings conflitantes.
- [x] Criar README com instalacao via Cargo, paths XDG, temas e controles.
- [ ] Gerar binario Linux release e smoke test em ambiente limpo.

### Experiência e prontidão de produto

- [x] Manter a jornada principal utilizável em 50x14 e apresentar uma mensagem
  acionável abaixo do tamanho mínimo.
- [x] Adaptar configurações, resultado e estatísticas sem cortar controles ou
  esconder atalhos em terminais compactos.
- [x] Explicar sessões automáticas sem pedir que o usuário escolha um exercício.
- [ ] Validar a primeira execução com usuários que nunca usaram o tuipe.
- [x] Testar todos os dez temas com Nerd Font e fallback Unicode, incluindo
  contraste e informação que não dependa somente de cor.
- [ ] Validar teclado, mouse, resize, colagem, IME, layouts não US e leitores de
  tela nos terminais suportados.
- [x] Projetar recuperação acionável para banco corrompido, migration interrompida
  e configuração inválida, sem exigir que o usuário encontre arquivos internos.
- [ ] Definir canal de feedback, notas de versão, compatibilidade de dados e
  estratégia de atualização antes da primeira release pública.

**Pronto quando:** o teste permanece responsivo sob carga, o terminal sempre e restaurado e a checklist de paridade esta fechada.

## 9. Estrategia de testes

### Unidade e propriedades

- Reducer de input, validacao e metricas com casos exatos.
- Propriedades: indices validos, probabilidades finitas/normalizadas, nenhuma palavra inexistente, determinismo por seed e nenhuma repeticao entre as duas palavras anteriores.
- Unicode: acentos NFC/NFD, graphemes e largura terminal.
- XP/level/streak copiados com vetores de referencia.

### Simulacao adaptativa

Manter cenarios deterministas:

1. sem historico -> distribuicao uniforme;
2. um erro isolado -> boost pequeno;
3. erros recorrentes em sessoes distintas -> subida em S;
4. muitos acertos posteriores -> recuperacao lenta;
5. AFK de segundos/minutos -> nenhuma penalidade de palavra;
6. palavra lenta por ser longa -> normalizacao correta;
7. mesmo teste repetido -> retornos decrescentes;
8. troca de pack -> skill compartilhada;
9. quote -> aprende, mas selecao da quote nao muda;
10. palavra extrema -> teto respeitado sem suprimir baseline natural.

Executar Monte Carlo com seeds fixas e intervalos de tolerancia, nao asserts em uma unica amostra aleatoria.

### TUI e integracao

- `ratatui::backend::TestBackend` + `insta` para snapshots.
- PTY/integration tests para raw mode, resize, mouse e restauracao apos panic.
- Golden tests de assets importados.
- Banco temporario por teste e migrations desde cada versao suportada.

Referencias: [Ratatui testing recipes](https://ratatui.rs/recipes/testing/), [Proptest](https://proptest-rs.github.io/proptest/proptest/index.html) e [Insta](https://insta.rs/).

## 10. Criterios finais de aceite

- Um usuario do Monkeytype reconhece imediatamente o fluxo e o feel.
- `time 30 / portuguese common / expert / arch` e visual e comportamentalmente equivalente ao uso de referencia.
- Nenhuma feature excluida reaparece por conveniencia tecnica.
- O app funciona integralmente offline e nao faz requests de rede.
- Configuracao e historico sobrevivem ao restart.
- Eventos brutos sao binarios/comprimidos, nunca um JSON por tecla.
- Adaptativo e imperceptivel no curto prazo, incremental no longo prazo e resistente a AFK.
- Adicionar word pack, quote pack ou tema nao exige modificar o motor.
- Input/render nao bloqueiam em persistencia ou recomputacao.
- Terminal e restaurado em quit, erro e panic.
- Todos os TODOs das fases 0–7 e seus criterios de pronto estao fechados.
