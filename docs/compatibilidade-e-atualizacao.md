# Compatibilidade e atualização

## Contrato da versão 0.1

A versão 0.1 usa:

- schema SQLite 9;
- codec de eventos brutos 3, com leitura das versões 1 a 3;
- métricas 2;
- modelo adaptativo 2;
- política de seleção 2.

O tuipe migra bancos antigos de forma incremental e transacional. Um binário
antigo nunca tenta reduzir um banco criado por versão mais nova: ele encerra com
uma mensagem explícita e deixa o arquivo intacto. Blobs desconhecidos também são
rejeitados sem apagar o histórico.

## Atualização segura

Antes de trocar o binário:

```sh
tuipe doctor
tuipe backup tuipe-antes-da-atualizacao.db
```

Depois da atualização, execute `tuipe doctor` novamente. Se uma nova versão das
métricas ou do modelo exigir rematerialização, use `tuipe rebuild`. Esse comando
parte da configuração, dos estímulos e dos eventos brutos persistidos; sessões
legadas sem estímulos continuam preservadas. A reconstrução adquire um bloqueio
de escrita antes de ler o histórico, rematerializa métricas e observações e só
então troca as habilidades derivadas; uma instância aberta nunca produz uma
projeção parcial.

A política adaptativa ativa e sua alternativa de rollback ficam versionadas no
banco. Em caso de regressão operacional, `tuipe policy rollback` troca as duas
atomicamente; `tuipe policy status` permite confirmar o estado antes e depois.
O fallback uniforme preserva a coleta de evidências e nunca apaga o histórico.
Nesse estado, a política alternativa roda em shadow mode: suas escolhas e
propensões ficam registradas separadamente na sessão, sem influenciar o texto
exibido. Isso permite comparar a candidata antes de restaurá-la.

## Reversão

Reinstalar um binário antigo pode falhar de forma segura se a versão já tiver
elevado o schema. Para uma reversão completa, restaure a cópia feita antes da
atualização junto com o binário anterior. Nunca substitua o banco atual sem
guardar uma cópia.

## Distribuição

A primeira release pública deve oferecer o pacote Cargo e um binário Linux
`x86_64` acompanhado de checksum SHA-256. Não haverá atualização automática na
série 0.1: o usuário escolhe quando atualizar, e as notas da versão devem indicar
qualquer migração ou necessidade de reconstrução. A publicação só deve ocorrer
depois que o repositório público e o canal de feedback existirem.

O artefato Linux é produzido por `scripts/build-portable-linux.sh` dentro de
Debian Bullseye, sem herdar a glibc recente da máquina de desenvolvimento. Ele
requer glibc 2.29 ou mais recente. O script usa `Cargo.lock`, separa o diretório
de build e gera checksums para o binário avulso e para um pacote com licença,
avisos, histórico de mudanças e proveniência dos conteúdos importados.
