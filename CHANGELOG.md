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
- rollback reversível da política adaptativa para um modo uniforme seguro;
- shadow mode que registra decisões candidatas sem alterar o teste apresentado;
- leitura retrocompatível dos estados adaptativos gravados antes da separação
  explícita de lentidão;
- atalhos configuráveis, mouse, terminais compactos e recuperação segura do
  terminal em saída, erro e sinais do sistema;
- detecção de Nerd Font no Kitty através do tmux, configurações reorganizadas e
  áreas clicáveis compatíveis com o desenho completo dos controles;
- navegação de configurações com foco explícito, setas direcionais simétricas,
  estados booleanos legíveis e legenda permanente dos comandos;
- painel mestre–detalhe para configurações largas, com resumo alinhado,
  descrição contextual, edição isolada e áreas de clique equivalentes;
- confirmação das configurações sem alteração implícita e explicação visível
  das regras normal, especialista e mestre;
- palavras prioritárias ordenadas pelo aumento adaptativo exibido, com a
  dificuldade interna usada apenas para desempate;
- ações do resultado com ícone, tecla e descrição reunidos no mesmo controle;
- navegação das estatísticas em abas delimitadas, responsivas e clicáveis, com
  estado ativo indicado sem preenchimento luminoso;
- eixos de gráficos com referências adaptativas, distribuição e atividade em
  colunas de escala explícita e barra de comandos separada por divisor;
- tendência suavizada sobre todas as tentativas válidas e prioridade adaptativa
  que ignora correções isoladas sem evidência suficiente.

### Validado

- paridade visual dos fluxos de 30 segundos, 50 palavras e citação;
- 148 testes automatizados em unidade, integração e pseudo-terminal;
- pacote Cargo reproduzível e binário Linux otimizado para glibc 2.29 ou mais
  recente, distribuído com licença e proveniência dos conteúdos;
- latência p99 abaixo de 1 ms para tecla e resize na máquina de validação.

### Pendente para 0.1.0

- primeira execução observada com pessoas que nunca usaram o tuipe;
- validação de IME, layout não US e leitor de tela nos terminais declarados;
- criação do repositório público e do canal de feedback.
