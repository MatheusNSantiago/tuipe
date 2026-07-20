# Registro de mudanças

Este projeto segue versionamento semântico. Mudanças incompatíveis no banco ou
nos eventos brutos exigem uma nova versão principal ou uma migração que preserve
os dados existentes.

## [Não lançado]

### Adicionado

- testes de digitação por tempo, quantidade de palavras e citações;
- currículo automático com prática representativa, direcionada, transferência,
  avaliação e retenção;
- estatísticas de progresso, histórico e diagnóstico de dificuldades;
- temas embutidos e pessoais, Nerd Font e fallback Unicode;
- banco SQLite privado, eventos compactados, backup, diagnóstico e reconstrução;
- atalhos configuráveis, mouse, terminais compactos e recuperação segura do
  terminal em saída, erro e sinais do sistema.

### Validado

- paridade visual dos fluxos de 30 segundos, 50 palavras e citação;
- 118 testes automatizados em unidade, integração e pseudo-terminal;
- pacote Cargo reproduzível e binário Linux otimizado;
- latência p99 abaixo de 1 ms para tecla e resize na máquina de validação.

### Pendente para 0.1.0

- primeira execução observada com pessoas que nunca usaram o tuipe;
- validação de IME, layout não US e leitor de tela nos terminais declarados;
- criação do repositório público e do canal de feedback.
