# Modelo de treinamento adaptativo do tuipe

Status: especificação de pesquisa e registro de implementação, 19 de julho de
2026.

Este documento define como o tuipe deve medir habilidade, escolher exercícios e
demonstrar progresso. Ele substitui a heurística provisória da seção 6 de
`PLAN.md`; os limites de produto do plano continuam válidos.

## Resumo executivo

O objetivo não é fazer o usuário obter um WPM alto em palavras que o próprio
sistema escolheu por serem fáceis ou familiares. O objetivo é aumentar a
velocidade sustentável, a precisão, a estabilidade e a capacidade de transferir
essas melhorias para texto novo.

Para isso, o tuipe deve separar quatro problemas que a implementação atual
mistura:

1. **medição:** estimar o que está difícil e com quanta certeza;
2. **intervenção:** estimar o que vale a pena treinar agora;
3. **sequenciamento:** montar uma sessão variada, espaçada e natural;
4. **avaliação:** medir progresso em material que o adaptativo não treinou.

Uma correção não aumenta a chance de uma palavra por um valor fixo. Duas,
dez ou cem correções também não significam nada sem saber quantas vezes a
palavra apareceu, em quais contextos, há quanto tempo, como foi selecionada e
qual era o estado do usuário. Dado tempo suficiente, contagens absolutas sempre
crescem. O sinal útil é o excesso de dificuldade por exposição, comparado a um
baseline contextual, com incerteza e viés de seleção explícitos.

A arquitetura recomendada é um modelo hierárquico e longitudinal:

- eventos brutos reconstruíveis são a fonte da verdade;
- cada exposição produz observações separadas de erro, correção e tempo;
- palavras, n-gramas e mecânicas compartilham evidência sem virarem a mesma
  habilidade;
- estados temporários, como aquecimento e fadiga, não contaminam a habilidade
  permanente;
- o currículo combina texto representativo, prática direcionada, exploração e
  transferência;
- testes-âncora independentes fornecem a série histórica comparável.

## O que significa digitar melhor

WPM sozinho é insuficiente. Um usuário pode aumentar WPM aceitando mais erros,
decorando um conjunto pequeno ou aproveitando um teste particularmente fácil.
O tuipe deve otimizar uma fronteira entre velocidade e precisão.

Os resultados principais são:

- **velocidade sustentável:** velocidade reproduzível acima de uma meta alta de
  precisão, e não o melhor pico;
- **precisão:** erros corrigidos e não corrigidos medidos separadamente;
- **eficiência:** resultado útil por tecla e por segundo, incluindo o custo das
  correções;
- **estabilidade:** pouca variação sem exigir ritmo mecanicamente uniforme;
- **retenção:** a melhoria permanece depois de horas e dias;
- **transferência:** a melhoria aparece em palavras e textos não treinados;
- **confiança comportamental:** o usuário sustenta velocidade em material novo,
  com baixa variabilidade e pouca dependência de correção.

Confiança subjetiva não pode ser inferida honestamente só pelos eventos do
teclado. Se um dia ela for necessária, deve vir de uma pergunta opcional e
esparsa. O produto pode mostrar confiança **comportamental**, deixando claro o
que foi medido.

## Evidência relevante

### Digitação é uma habilidade hierárquica

O modelo de dois laços de Logan e Crump trata a palavra como interface entre um
laço externo, lexical e perceptual, e um laço interno que transforma letras em
movimentos e teclas. Experimentos também encontram efeitos diferentes de
frequência da palavra, frequência do dígrafo, fronteira silábica e dificuldade
física sobre o intervalo entre teclas.

Consequência: demora antes da primeira tecla, intervalos internos, erro lexical
e transição física não devem ser comprimidos em um único “tempo da palavra”.
Uma palavra rara pode ser lenta por leitura; um dígrafo comum pode ser lento por
execução motora; ambos podem produzir o mesmo tempo total.

### WPM médio esconde mecanismos importantes

O estudo de 136 milhões de teclas de Dhakal et al. encontrou grande variação
entre pessoas e relações fortes entre velocidade, rollover, intervalos entre
teclas e tipos de erro. Feit et al. mostraram que usuários autodidatas podem
alcançar desempenho de usuários de touch typing e que consistência do mapeamento
dedo-tecla e pouco movimento global das mãos explicam mais do que simplesmente
usar um número “correto” de dedos.

Consequência: o tuipe não deve diagnosticar técnica dos dedos a partir de texto
digitado nem impor uma técnica canônica que não consegue observar. Deve medir o
resultado e as transições disponíveis de forma portátil.

### Correção é comportamento, custo e evidência — não uma falha binária

As métricas de Soukoreff e MacKenzie distinguem erros corrigidos, erros deixados
no texto, teclas corretivas e teclas desperdiçadas. Outros experimentos mostram
que parte da detecção e correção pode ocorrer sem relato consciente. Portanto,
backspace não é sinônimo de “esta palavra é difícil” e ausência de backspace não
é sinônimo de domínio.

Consequência: é necessário reconstruir o fluxo de edição. Só deve contar como
erro corrigido uma edição que de fato removeu entrada divergente. Backspace
preventivo, `Ctrl+W` numa entrada correta e edição de separador são eventos
diferentes.

### Tempo de treino não é igual a aprendizagem

Aprendizagem motora distingue desempenho durante a prática de retenção e
transferência posteriores. Prática bloqueada e feedback frequente podem deixar
a sessão bonita sem produzir a melhor retenção. Prática espaçada, variável e
intercalada frequentemente piora o desempenho imediato, mas pode melhorar
retenção e transferência. O efeito depende da habilidade do aluno e da
dificuldade da tarefa; resultados de laboratório não autorizam uma regra única
para toda pessoa ou tarefa.

Consequência: repetir imediatamente a mesma palavra até acertar é um recurso
diagnóstico curto, não o currículo. O sistema deve espaçar e reencontrar a mesma
transição em palavras diferentes.

### Desafio precisa ser ajustado, mas “85%” não é uma lei da digitação

O Challenge Point Framework prevê maior aprendizado quando a dificuldade
funcional combina a dificuldade da tarefa com a habilidade atual. A chamada
“regra dos 85%” foi derivada para classes específicas de aprendizagem
perceptual/classificação. Erros de digitação têm custos assimétricos e o domínio
é diferente.

Consequência: o tuipe não deve deliberadamente derrubar a precisão global para
85%. Uma faixa alta, inicialmente 97–99%, é uma hipótese de produto mais
coerente para fluência, mas precisa ser calibrada com retenção e transferência.
O desafio pode vir da velocidade e da composição do texto, não da fabricação de
erros.

## Modelo causal da observação

Uma observação de digitação resulta de vários componentes:

```text
habilidade estável
  ├─ habilidade lexical por idioma e palavra
  ├─ habilidade motora por layout e n-grama
  └─ mecânicas: acento, dead key, caixa e pontuação
           +
estado temporário da sessão
  ├─ aquecimento, fadiga, pausa e hora do dia
  ├─ teclado/layout/perfil de ambiente
  └─ pressão de velocidade e modo do teste
           +
dificuldade do estímulo
  ├─ frequência, tamanho, posição e contexto
  └─ pontuação, caixa, número e familiaridade recente
           +
política de seleção
  └─ normal, direcionada, exploração ou transferência
           ↓
eventos, erros, correções e latências observados
```

O modelo precisa representar esses componentes porque confundi-los produz
intervenções erradas. Uma sessão cansada não deve condenar permanentemente as
palavras daquele minuto; trocar de teclado não deve parecer regressão lexical;
ver uma palavra muitas vezes por causa do adaptativo não deve aumentar sua
“importância” por si só.

## Dados necessários

### Sessão

Cada sessão deve congelar:

- versões do aplicativo, métricas, codec, modelo e política;
- idioma, word pack, modo, duração/quantidade, dificuldade e modificadores;
- seed e identidade do conjunto de estímulos;
- início, fim, estado terminal e tempo monotônico total;
- perfil de ambiente informado: layout lógico, teclado/perfil físico e versão;
- política ativa e parâmetros efetivos;
- perda de foco, resize, paste, repetição automática de tecla e sinais de IME;
- se é prática, avaliação-âncora, transferência ou repetição deliberada.

O perfil físico deve ser simples e manual. Um terminal não identifica de forma
portátil o teclado real. Quando o usuário troca de perfil, a habilidade lexical
pode ser compartilhada, mas o baseline motor não.

### Seleção de cada token

Para cada palavra ou token apresentado:

- texto original, forma NFC, forma lexical e idioma;
- origem no corpus, frequência/rank e identificador do pack;
- tamanho em grafemas; n-gramas; presença de caixa, pontuação, número, acento e
  dead key conhecida pelo perfil;
- posição e contexto anterior/posterior;
- instante em que ficou visível, instante em que virou o token ativo e posição
  no viewport, pois o usuário pode preparar palavras futuras;
- componente que o escolheu: representativo, direcionado, exploração,
  transferência ou repetição;
- peso e probabilidade exata de seleção no instante do sorteio;
- candidatos elegíveis e versão da política, ou dados suficientes para
  reconstruí-los.

Registrar a probabilidade de seleção é obrigatório. O adaptativo decide os
dados que observará; sem essa propensão não é possível distinguir dificuldade
real de superamostragem nem avaliar uma política alternativa honestamente.
Uma repetição iniciada manualmente pelo usuário não tem uma propensão de sorteio
útil: deve ser marcada como intervenção do usuário e excluída de estimativas
representativas, embora seus eventos ainda possam informar execução.

### Eventos brutos

O fluxo mínimo por evento contém:

- delta monotônico desde o evento anterior;
- tipo: inserção, backspace, apagar palavra, commit, restart, finish ou fail;
- grafema esperado e recebido, índice do token e posição lógica;
- entrada antes/depois ou uma operação suficiente para reconstruí-la;
- modificadores, origem do evento e flags de paste/repeat/IME quando disponíveis;
- correção no instante do evento sem apagar o histórico posterior.

Key-up, duração da tecla e rollover só devem ser usados quando a plataforma os
fornecer de forma confiável. Eles enriquecem o modelo, mas não podem ser
requisito para o tuipe funcionar em qualquer terminal.

### Exposição derivada

O materializador reconstrói, para cada exposição:

- completa, parcial, censurada, falha terminal ou abandonada;
- entrada final e caminho de edição;
- substituições, omissões, inserções, transposições e repetições;
- erros corrigidos e não corrigidos;
- quantidade e custo temporal de teclas corretivas e desperdiçadas;
- latência até a primeira tecla;
- tempo de pré-visualização antes da ativação e tempo ativo antes da primeira
  tecla;
- sequência de intervalos internos entre teclas;
- tempo de execução fluente e probabilidade de cada pausa ser interrupção;
- desaceleração após erro e tempo até a correção;
- n-gramas e mecânicas envolvidos;
- contexto da sessão e propensão que gerou a exposição.

Dados derivados continuam reconstruíveis. O evento bruto versionado é a fonte
da verdade; tabelas de observação e habilidade são projeções consultáveis.

## Inferência de dificuldade

### 1. Erros condicionados a exposições

Para cada componente, o tuipe estima separadamente:

- probabilidade de erro não corrigido;
- probabilidade de erro corrigido;
- custo da correção;
- probabilidade de sucesso limpo.

Um modelo hierárquico logístico é a referência conceitual:

```text
logit P(erroᵢ) = baseline do usuário/idioma
               + efeito da palavra
               + efeitos dos n-gramas
               + efeitos das mecânicas
               + contexto da exposiçãoᵢ
               + estado temporário da sessãoᵢ
```

Palavras com poucas exposições ficam próximas do prior compartilhado por
frequência, tamanho e n-gramas. Só se afastam quando acumulam evidência. Isso é
partial pooling: uma correção em uma palavra vista uma vez não vira 100% de
dificuldade, e duas correções em mil exposições não viram prioridade.

Exposições próximas e idênticas são correlacionadas. O tamanho efetivo da
amostra deve crescer menos quando a mesma palavra reaparece na mesma sessão e
no mesmo contexto, em vez de aplicar um desconto arbitrário a todo teste
repetido. A propensão serve para métricas representativas e avaliação de
políticas; ela não deve apagar uma execução observada nem transformar
automaticamente `1 / propensão` no peso da atualização individual.

A decisão usa um **efeito mínimo relevante**, e não apenas “alguma diferença”:

```text
sinal de erro = P(excesso de erro > δ_erro | dados)
              × magnitude esperada do excesso
              × custo do tipo de erro
```

`δ_erro` é uma diferença pequena o bastante para importar na prática, calibrada
por dados de retenção. Erro não corrigido recebe custo maior que erro corrigido;
correção recebe também seu custo de tempo. Nenhum deles recebe pontos fixos por
ocorrência.

### 2. Latência contextual e robusta

O tempo não deve ser dividido apenas pelo número de grafemas. O esperado varia
com posição, frequência da palavra e dos dígrafos, mão/transição inferível pelo
layout, caixa, pontuação e estado da sessão.

Intervalos entre teclas são assimétricos, têm cauda longa e podem formar uma
mistura entre execução fluente e hesitação. Um limiar universal, como 2 s, perde
mais dados justamente de iniciantes e pessoas variáveis. O modelo de referência
é uma mistura personalizada:

```text
log(IKIᵢ) ~ P(fluenteᵢ) × distribuição fluente contextual
          + P(hesitaçãoᵢ) × distribuição de pausa contextual
```

O sinal motor vem do resíduo da componente fluente. A primeira tecla e pausas
prováveis são sinais separados. Para execução online inicial, uma aproximação
robusta com quantis e MAD por contexto é aceitável, desde que mantenha a
probabilidade de pausa e possa ser substituída sem perder eventos.

No teste de palavras, o usuário enxerga tokens futuros. A latência da primeira
tecla mede uma combinação de planejamento prévio e troca para o token ativo.
Sem `visible_at`, `activated_at` e posição no viewport ela não pode ser
interpretada como acesso lexical puro.

### 3. Hierarquia de habilidades

- **palavra lexical:** idioma + forma NFC; `esta` e `está` são palavras
  diferentes;
- **n-grama motor:** layout/perfil + sequência; compartilha evidência entre
  palavras;
- **mecânica:** caixa, pontuação e sequência de composição/dead key;
- **contexto entre palavras:** somente depois de haver dados em contextos
  diferentes; não deve dominar o início frio.

Um n-grama só recebe sinal generalizável depois de aparecer em palavras
distintas. Repetir a mesma palavra não prova que o n-grama é a causa. De modo
simétrico, uma palavra pode continuar difícil por acesso lexical mesmo com seus
n-gramas motores dominados.

### 4. Estado longitudinal

Habilidade muda gradualmente; estado da sessão muda rapidamente. Um modelo de
estado deve:

- atualizar habilidade com ganho pequeno e incerteza explícita;
- permitir aprendizagem e esquecimento lentos;
- estimar aquecimento/fadiga sem gravá-los como dificuldade permanente;
- aumentar a necessidade de revisão com o tempo, sem restaurar um erro antigo
  como verdade;
- segmentar habilidade motora quando o perfil de teclado/layout muda.

### 5. Dificuldade não é prioridade

Uma palavra pode ser difícil e ainda assim ser um exercício ruim: talvez seja
raríssima, cubra uma habilidade já treinada melhor por outra palavra ou tenha
incerteza causada por uma única pausa. A prioridade de treino é utilidade
esperada:

```text
prioridade = necessidade posterior
           × ganho de transferência esperado
           × valor de retenção
           × relevância no idioma/objetivo
           × fator de diversidade
           + exploração limitada pela incerteza
```

O sorteio pode usar amostragem da posterior para explorar, mas somente dentro de
um orçamento limitado. Um bandit guloso otimiza a recompensa imediata que ele
mesmo influencia e pode prender o currículo em itens fáceis ou ruidosos.

## Currículo e sequenciamento

O gerador deve montar cada sessão a partir de quatro componentes:

1. **representativo:** amostra natural do corpus; preserva fluência geral;
2. **direcionado:** exercita habilidades com maior utilidade esperada;
3. **exploração:** reduz incerteza em hipóteses plausíveis;
4. **transferência:** usa palavras novas que compartilham a habilidade-alvo.

Uma política experimental inicial pode reservar aproximadamente 55%, 25%, 10%
e 10% para esses componentes. Esses valores são ponto de partida seguro, não
resultado científico. Devem ser calibrados por retenção, transferência,
cobertura e experiência subjetiva.

Restrições do sequenciador:

- manter palavras reais e texto natural; sequências sem sentido ficam restritas
  a sondas diagnósticas opcionais;
- combinar fluxos de palavras, úteis para prática motora concentrada, com frases
  e citações naturais nas avaliações de transferência;
- não concentrar uma palavra em bloco só porque ela falhou;
- espaçar a mesma palavra entre sessões e exercitar seus n-gramas em palavras
  diferentes no intervalo;
- limitar contribuição por palavra e por habilidade para preservar diversidade;
- aumentar dificuldade gradualmente conforme a precisão sustentável;
- diminuir desafio quando há degradação persistente, sem reagir a um único erro;
- garantir cobertura de palavras comuns e nunca excluir silenciosamente o
  restante do idioma;
- registrar a probabilidade final após todas as restrições.

A chance de uma palavra aparecer na sessão deve vir de simulação ou cálculo do
sequenciador real. `1 - (1 - p)^n` não é exato quando há exclusão das palavras
anteriores, mistura de componentes e limites de diversidade.

## Avaliação sem contaminação

O treino adaptativo não pode avaliar a si próprio. O tuipe precisa de três
protocolos separados:

### Avaliação-âncora

Conjuntos estratificados equivalentes por idioma, frequência, tamanho e
n-gramas, selecionados sem usar dificuldades individuais. Formas equivalentes
rotacionam para reduzir memorização. Resultados âncora formam a série histórica
principal.

### Transferência

Palavras e frases não usadas diretamente no treino, mas que compartilham
componentes. Mede se o aprendizado de uma transição se generalizou.

### Retenção

Sondas depois de intervalos crescentes — por exemplo, outra sessão, outro dia e
outra semana — sem repetição imediata anterior. Mede se a melhora permaneceu.

Recordes e progresso devem comparar configurações compatíveis. Um PB em prática
adaptativa não deve ser apresentado como melhora global.

## Métricas e estatísticas úteis

### Resultado da sessão

- WPM e WPM bruto;
- precisão total, erro corrigido e erro não corrigido;
- teclas por caractere e custo de correção;
- consistência e quantis de intervalo entre teclas;
- duração ativa, pausa provável e tempo total;
- origem/composição do teste;
- marcação de resultado de prática, âncora ou transferência.

### Progresso

- WPM sustentável em avaliações-âncora numa faixa de precisão alta;
- fronteira velocidade–precisão, não apenas duas médias independentes;
- precisão corrigida e não corrigida;
- eficiência líquida após correções;
- retenção e transferência;
- ganho por minuto ativo;
- intervalo de incerteza e quantidade de avaliações comparáveis.

### Diagnóstico de habilidades

Para cada palavra, n-grama ou mecânica relevante:

- excesso posterior de erro e latência sobre o baseline;
- exposições efetivas, contextos distintos e recência;
- incerteza, tendência e retenção estimada;
- exemplos de palavras de transferência;
- chance real de aparecer na próxima sessão e o motivo da seleção.

O histórico recente não deve ser uma tabela de testes heterogêneos sem contexto.
Ele deve permitir distinguir prática de avaliação e explicar qualquer mudança
de configuração. Gráficos de progresso usam apenas pontos comparáveis; prática
adaptativa aparece numa série separada.

## Casos extremos e revisão adversarial

| Caso | Erro ingênuo | Tratamento correto |
| --- | --- | --- |
| Duas correções após mil exposições | marcar prioridade | taxa posterior fica próxima do baseline |
| Uma correção na primeira exposição | concluir dificuldade | prior domina; no máximo gera exploração leve |
| Adaptativo repete a palavra | contar tudo como confirmação | usar propensão, recência e contextos distintos |
| `Ctrl+W` apaga palavra correta | contar erro corrigido | reconstruir se havia divergência antes da edição |
| Tempo termina no meio da palavra | contar falha completa | observação censurada; aproveitar apenas eventos observáveis |
| `expert` termina no primeiro erro | considerar o restante correto | preservar causa e marcar restante como não observado |
| Pausa longa por distração | tornar palavra lenta | mistura de pausas + foco; manter classificação incerta |
| Iniciante é naturalmente lento | remover muitos IKIs como AFK | modelo de pausa personalizado, sem limiar universal |
| Palavra rara demora antes da primeira tecla | culpar n-grama | separar acesso lexical de execução interna |
| Troca de teclado/layout | registrar regressão geral | novo perfil motor; lexical pode continuar compartilhado |
| Sessão cansada | reduzir habilidade permanente | estado temporário de fadiga com baixa influência longitudinal |
| Mesma palavra fica rápida por repetição | declarar transferência | testar n-grama em palavras novas e depois de intervalo |
| Paste/IME entrega vários grafemas | inferir IKIs zero | marcar origem e excluir de inferência motora |
| Tecla repetida pelo sistema | inferir duplicação voluntária | usar flag de repeat quando disponível |
| Acento composto em eventos diferentes | tratar código por código | comparar grafemas NFC e modelar mecânica separadamente |
| Usuário força velocidade e erra mais | chamar de regressão | atualizar a fronteira velocidade–precisão |
| Palavra comum tem baseline natural alto | superamostrar ainda mais | medir excesso sobre frequência natural e limitar boost |
| Política nova parece melhor nos próprios dados | aceitar melhoria | avaliação contrafactual com propensão + teste prospectivo |
| Sessão de 1–2 s | exibir WPM como progresso | persistir, mas excluir de agregados sem amostra mínima |

Outras ameaças à validade:

- digitação de cópia não mede toda a escrita espontânea;
- desempenho dentro da sessão pode refletir aquecimento, não aprendizagem;
- feedback imediato pode criar dependência e alterar a estratégia;
- usuários diferentes aceitam custos de erro diferentes;
- resultados de memória, classificação e esportes informam o desenho, mas não
  provam parâmetros exatos para digitação;
- um modelo sofisticado não corrige eventos incompletos ou rótulos errados.

## Validação do modelo

Antes de influenciar palavras, cada versão deve operar em **shadow mode**:
estima prioridades e previsões sem mudar o teste. A promoção exige:

1. calibração: grupos previstos com 5%, 10% e 20% de erro apresentam taxas
   próximas dessas previsões;
2. discriminação: dificuldades altas predizem erros/latências futuras melhor que
   frequência e tamanho sozinhos;
3. estabilidade: uma única sessão ruim não reorganiza todo o currículo;
4. transferência: treino direcionado melhora palavras novas relacionadas;
5. retenção: o ganho permanece depois de um intervalo;
6. segurança do currículo: cobertura, diversidade, repetição e precisão ficam
   dentro dos limites;
7. auditoria: toda prioridade pode ser explicada a partir de eventos e versões.

Métodos de validação:

- testes de propriedade para reconstrução de edição e normalização Unicode;
- simuladores com perfis sintéticos, mudanças de habilidade, fadiga e ruído;
- replay determinístico das sessões brutas;
- gráficos de calibração e resíduos por contexto;
- comparação temporal controlada no mesmo usuário, alternando políticas;
- avaliação prospectiva; propensão ajuda análise offline, mas não substitui um
  teste real quando a política muda muito.

## Estado da implementação

O modelo ativo já possui:

- eventos brutos v2 com caminho de inserção/remoção, foco, paste e causa
  terminal, persistidos também em restart e saída;
- seed, estímulos, tipo de sessão, versão da política, componente de seleção e
  propensão congelados por sessão;
- validação semântica do replay antes da reconstrução transacional das projeções;
- censura explícita e tempos separados de planejamento, execução fluente,
  correção e interrupção;
- posterior beta por exposição, prior pessoal, efeito mínimo relevante e
  baseline de latência por comprimentos próximos;
- habilidade lexical, n-gramas e mecânicas materializados separadamente;
- generalização de padrão somente depois de palavras distintas;
- currículo misto 55/25/10/10, piso representativo, exploração de incerteza,
  propensão final e simulação do sequenciador real;
- espaçamento longitudinal separado da dificuldade, sem restaurar erro antigo;
- holdout de transferência, avaliações-âncora estratificadas e sondas de
  retenção, todos agendados automaticamente;
- estatísticas de progresso baseadas em avaliações comparáveis e diagnóstico de
  palavras e padrões;
- simulador determinístico de 2.000 sessões cobrindo aprendizado, diversidade e
  teto de exposição.

Limitações ainda abertas:

- o rebuild valida os eventos brutos, mas refaz as habilidades a partir das
  observações consultáveis; ainda não rematerializa essas observações somente do
  blob bruto;
- ativação e primeira visibilidade do token não são eventos do motor, portanto o
  planejamento anterior à primeira palavra não é observável;
- repeat/IME/key-up dependem do que o terminal entrega e ainda não possuem flags
  portáteis completas;
- aquecimento e fadiga ainda não são um estado latente separado; pausas são
  classificadas robustamente, mas uma degradação contínua pode alcançar a
  posterior;
- não há perfil físico automático, pois terminais não expõem o teclado real;
- calibração prospectiva, intervalos de incerteza no painel, fronteira
  velocidade–precisão e rollback de política ainda não estão prontos.

## Plano de implementação

### Fase 1 — fonte da verdade

- [x] Versionar `RawEvent` v2 com operação, posição, origem e contexto disponível.
- [x] Persistir eventos em todos os estados terminais, inclusive restart e saída.
- [x] Congelar estímulos, seed, tipo e política por sessão.
- [x] Registrar token, componente e propensão por seleção.
- [ ] Registrar perfil de ambiente e flags portáteis de repeat/IME.

### Fase 2 — reconstrução e métricas de entrada

- [x] Reconstruir e validar caminhos de edição de modo determinístico.
- [x] Classificar erros corrigidos/não corrigidos e censura.
- [x] Separar planejamento, IKI fluente, interrupção e custo de correção.
- [x] Cobrir Unicode, paste e `Ctrl+W` sem inventar eventos indisponíveis.
- [ ] Rematerializar observações consultáveis somente a partir do evento bruto.

### Fase 3 — baselines e modelo em shadow mode

- [ ] Criar perfis de ambiente sem exigir configuração manual do usuário.
- [x] Materializar posterior de palavra, n-grama e mecânica.
- [ ] Separar aquecimento/fadiga da habilidade longitudinal.
- [x] Preservar dados v1 sem atribuir evidência fictícia ao modelo v2.
- [ ] Validar calibração e valor preditivo prospectivamente.

### Fase 4 — avaliação e estatísticas

- [x] Introduzir sessões-âncora equivalentes, retenção e transferência.
- [x] Separar séries de prática e avaliação.
- [ ] Mostrar fronteira velocidade–precisão e intervalos de incerteza.
- [x] Exibir diagnóstico explicável de palavras, n-gramas e mecânicas.

### Fase 5 — currículo adaptativo v2

- [x] Implementar a mistura representativa/direcionada/exploração/transferência.
- [x] Aplicar espaçamento, diversidade, holdout e limites de cobertura.
- [x] Calcular chance de sessão com o sequenciador real.
- [ ] Manter shadow mode e rollback operacional por versão.
- [ ] Calibrar parâmetros por ganho retido e transferido com dados prospectivos.

Dados históricos do adaptativo v1 podem continuar visíveis, mas não possuem
informação suficiente para reconstruir a posterior completa. Não devem ganhar
precisão fictícia numa migração.

## Decisões rejeitadas

- limiar de N correções ou N erros;
- pontos fixos somados por evento;
- taxa bruta sem prior, contexto e incerteza;
- priorizar a palavra com maior WPM residual sem separar primeira tecla;
- repetir a palavra até acertar e chamar isso de domínio;
- usar 85% de precisão como alvo universal;
- medir progresso no mesmo conjunto escolhido pelo adaptativo;
- bandit guloso sem propensão, holdout e limites de currículo;
- descartar qualquer intervalo acima de um limiar universal;
- inferir técnica dos dedos sem sensores capazes de observá-la;
- apresentar uma chance aproximada como se fosse probabilidade exata.

## Referências primárias

- Dhakal, Feit, Kristensson e Oulasvirta, [Observations on Typing from 136
  Million Keystrokes](https://userinterfaces.aalto.fi/136Mkeystrokes/resources/chi-18-analysis.pdf),
  CHI 2018.
- Feit, Weir e Oulasvirta, [How We Type: Movement Strategies and Performance in
  Everyday Typing](https://userinterfaces.aalto.fi/how-we-type/resources/HowWeType_CHI16.pdf),
  CHI 2016.
- Soukoreff e MacKenzie, [Metrics for Text Entry Research](https://www.yorku.ca/mack/chi03.pdf),
  CHI 2003.
- Crump e Logan, [Hierarchical Control and Skilled Typing](https://www.crumplab.com/publications/Crump/files/4704/Crump%20and%20Logan%20-%202010%20-%20Hierarchical%20control%20and%20skilled%20typing%20Evidence%20.pdf),
  2010.
- Gentner, Larochelle e Grudin, [Lexical, Sublexical, and Peripheral Effects in
  Skilled Typewriting](https://doi.org/10.1016/0010-0285%2888%2990015-1), 1988.
- Keith e Ericsson, [A Deliberate Practice Account of Typing Proficiency in
  Everyday Typists](https://clinica.ispa.pt/sites/default/files/12_-_a_deliberate_practice_account_of_typing_proficiency_in_everyday_typists.pdf),
  2007.
- Crump e Logan, [Warning: This Keyboard Will Deconstruct](https://pubmed.ncbi.nlm.nih.gov/20551364/),
  2010.
- Pinet e Nozari, [Correction Without Consciousness in Complex Tasks: Evidence
  from Typing](https://journalofcognition.org/articles/202), 2022.
- Van Waes et al., [Modelling Typing Disfluencies as a Finite Mixture
  Process](https://link.springer.com/article/10.1007/s11145-021-10203-z), 2021.
- Schmidt e Bjork, [New Conceptualizations of Practice](https://bjorklab.psych.ucla.edu/wp-content/uploads/sites/13/2016/07/Schmidt_RBjork_1992.pdf),
  1992.
- Guadagnoli e Lee, [Challenge Point: A Framework for Conceptualizing the Effects
  of Various Practice Conditions in Motor Learning](https://doi.org/10.1080/02701367.2004.10609184),
  2004.
- Wilson et al., [The Eighty Five Percent Rule for Optimal Learning](https://pmc.ncbi.nlm.nih.gov/articles/PMC6831579/),
  2019.
- Mettler, Massey e Kellman, [A Comparison of Adaptive and Fixed Schedules of
  Practice](https://pubmed.ncbi.nlm.nih.gov/27123574/), 2016.
- Swaminathan e Joachims, [Counterfactual Risk Minimization](https://arxiv.org/abs/1502.02362),
  2015.
- Silverman et al., [Using Words Instead of Jumbled Characters as Stimuli in
  Keyboard Training Facilitates Fluent Performance](https://pmc.ncbi.nlm.nih.gov/articles/PMC3251293/),
  2012.

As referências de aprendizagem motora e adaptativa sustentam princípios como
espaçamento, desafio, incerteza e avaliação independente. Elas não determinam
sozinhas os coeficientes do tuipe. Esses coeficientes continuam sendo hipóteses
de produto que precisam passar pela validação descrita acima.
