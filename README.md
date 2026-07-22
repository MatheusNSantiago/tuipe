<h1 align="center">tuipe</h1>

<p align="center">
   O tuipe é um programa de treinamento de digitação inspirado no <a href="https://monkeytype.com/">Monkeytype</a> que roda inteiramente no terminal. É 100% local e não requer conta, servidor ou telemetria. O foco é treinar digitação em diferentes vocabulários, com estatísticas detalhadas e um modelo adaptativo que aprende com seus erros.
</p>

<p align="center">
  <img src="assets/readme/digitacao.gif" alt="Uma sessão de digitação no tuipe" width="1200">
</p>

## Treino adaptativo

Esse modo (ligado por default) é feito para que os testes foquem nas palavras que você mais precisa treinar.

Para isso, são analisados vários aspectos do seu desempenho:

- **Detecta palavras lentas:** uma palavra pode pedir treino mesmo quando você
  consegue corrigi-la antes de confirmar.
- **Detecta erros em sequências:** se você erra `criança` e `França`, o modelo reforça palavras com
  `nça`, sem culpar todas as sequências das duas palavras.
- **Correções também contam:** apagar uma letra pesa pouco; reconstruir quase
  toda a palavra e gastar vários segundos corrigindo pesa muito mais.
- **Identifica AFK:** o tempo detectado como ausência não entra no aprendizado.
- **Percebe quando você volta a acertar:** a frequência extra da palavra diminui
  gradualmente até níveis normais.

Todos esses sinais são combinados em um modelo de prioridade que decide quais palavras aparecerão mais vezes nos testes.

## O teste

Estão disponíveis testes por tempo, quantidade de palavras e citações, com:

- português e inglês;
- vocabulários comum, 1k e 5k;
- pontuação e números;
- modos normal, especialista e mestre;
   * modo normal: permite erros e correções
   * modo especialista: a primeira palavra errada encerra o teste (pode corrigir sem problema)
   * modo mestre: o primeiro caractere incorreto encerra o teste

## Estatísticas

A visão geral separa tentativas válidas de sessões interrompidas ou muito fora
do seu ritmo. O gráfico usa todo o histórico comparável, mostra a tendência de
WPM e mantém erros individuais visíveis.

Logo abaixo aparecem as palavras e os padrões que mais pedem treino. A coluna
de prioridade mostra diretamente quanto o modelo aumentou sua presença, como
`+2%`. A tela também mostra contagens concretas, como `falhou 1/5`, `corrigiu
4/5` e quantos caracteres foram apagados durante essas recuperações.

<p align="center">
  <img src="assets/readme/estatisticas.webp" alt="Visão geral das estatísticas do tuipe" width="1200">
</p>

Cada palavra pode ser aberta para entender o diagnóstico. O tuipe mostra
falhas, correções, exposições, ritmo contra a sua base, tentativas recentes e os
padrões relacionados.

<p align="center">
  <img src="assets/readme/detalhe-palavra.webp" alt="Diagnóstico de uma palavra no tuipe" width="1200">
</p>

O progresso também pode ser visto como distribuição de WPM e atividade diária.
Uma sessão individual preserva o texto praticado e aponta o que mais exigiu
atenção.

<p align="center">
  <img src="assets/readme/progresso.webp" alt="Distribuição de WPM e atividade diária" width="1200">
</p>

<p align="center">
  <img src="assets/readme/detalhe-sessao.webp" alt="Detalhe de uma sessão concluída" width="1200">
</p>

## Configuração sem sair do fluxo

`esc` abre um painel mestre e detalhe. As setas verticais escolhem a preferência,
as horizontais alteram seu valor e `enter` confirma. Cada opção explica apenas o
que está selecionado naquele momento.

As alterações são salvas automaticamente. A mesma tela funciona com teclado e
mouse e se reorganiza em terminais menores.

<p align="center">
  <img src="assets/readme/configuracoes.webp" alt="Configurações do tuipe" width="1200">
</p>

## Local de verdade

O tuipe não possui conta, servidor ou telemetria. Configuração, histórico e
modelo ficam na máquina do usuário seguindo os diretórios XDG.

Os eventos de cada sessão são serializados com Postcard, comprimidos com Zstd e
armazenados em SQLite. Métricas e habilidades são projeções reconstruíveis. Se
uma fórmula mudar durante o desenvolvimento, `tuipe rebuild` pode recalcular o
estado derivado a partir dos eventos brutos.

Outros comandos úteis:

```sh
tuipe doctor                 # valida banco, eventos e configuração
tuipe backup                 # cria uma cópia consistente do SQLite
tuipe rebuild                # recalcula métricas e o modelo pelos eventos brutos
```

## Instalação

O projeto ainda não teve sua primeira versão pública. Hoje, a forma mais simples
de executar é pelo código fonte:

```sh
git clone https://github.com/MatheusNSantiago/tuipe.git
cd tuipe
cargo run --release
```

Requisitos:

- Linux x86-64;
- Rust 1.88 ou mais recente;
- terminal UTF-8 com pelo menos 50 colunas por 14 linhas;
- Nerd Font recomendada, mas não obrigatória.

Para instalar o binário localmente:

```sh
cargo install --path . --locked
tuipe
```

## Controles principais

- `esc`: configurações;
- `ctrl+s`: estatísticas;
- `ctrl+w` ou `ctrl+backspace`: apagar a palavra atual;
- `ctrl+c`: cancelar o teste atual;
- `enter`: próximo teste;
- `r`: repetir o mesmo teste na tela de resultado;
- `q`: sair nas telas com esse controle visível.

Os controles disponíveis sempre aparecem no rodapé da tela. Atalhos de
aplicação podem ser alterados em `config.toml` usando a sintaxe da crate
[Crokey](https://docs.rs/crokey/).

## Origem e implementação

O commit `781dcd9fe66fb4d8fb3f5e408d1b057a2054b9d5` do Monkeytype é a
referência congelada para comportamento, métricas, conteúdo e decisões visuais.
O tuipe traduz esse fluxo para Rust, Ratatui e uma grade de células sem tentar
portar o restante do produto.

A origem e a licença dos conteúdos importados estão registradas em [NOTICE](NOTICE).

Validação local completa:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
cargo bench --bench latencia_input_render
```

## Licença

[GPL-3.0-only](LICENSE)
