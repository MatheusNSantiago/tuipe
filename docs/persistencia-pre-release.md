# Persistência antes da primeira versão

O tuipe ainda não possui uma versão pública. Portanto, existe um único formato
de persistência válido: o formato produzido pelo código atual.

## Contrato atual

- schema SQLite 10;
- codec de eventos brutos 3;
- métricas 2;
- modelo adaptativo 2;
- política de seleção 3.

Ao criar um banco, o tuipe instala diretamente esse schema. Ao abrir um banco
existente, exige a mesma versão exata. Um schema, blob de eventos ou estado do
modelo com outro formato é rejeitado sem conversão e sem alteração do arquivo.
Durante o desenvolvimento, a ação correta é apagar o banco incompatível e
deixar o aplicativo recriá-lo.

Isso não se aplica a corrupção. Se o SQLite comprovar que o arquivo está
corrompido, o tuipe preserva o arquivo em quarentena e cria um armazenamento
vazio. Erros de permissão, disco ou schema incompatível continuam visíveis; não
são classificados como corrupção para esconder o problema.

## Ferramentas de integridade

`tuipe doctor` abre o banco somente para leitura e valida o schema atual, a
integridade do SQLite, os eventos brutos, os estados adaptativos e a proveniência
das sessões. `tuipe backup` produz uma cópia consistente inclusive quando o WAL
está ativo. `tuipe rebuild` rematerializa métricas e habilidades a partir dos
eventos atuais; ele não converte formatos antigos.

O rollback da política adaptativa não é migração de dados. A política ativa e a
candidata em shadow mode pertencem ao mesmo schema e podem trocar de papel
atomicamente para controlar experimentos.

## Depois da primeira versão pública

Compatibilidade começa no primeiro formato efetivamente distribuído. Antes de
alterá-lo, será necessário definir quais versões são suportadas, como upgrades e
reversões funcionam e por quanto tempo cada formato é aceito. Essas decisões não
devem criar hoje caminhos de código para usuários que ainda não existem.
