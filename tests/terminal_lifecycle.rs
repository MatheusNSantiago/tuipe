#![cfg(unix)]

use std::{
    io::{Read, Write},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use portable_pty::{Child, CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};

const INICIO_COLAGEM: &[u8] = b"\x1b[?2004h";
const FIM_COLAGEM: &[u8] = b"\x1b[?2004l";
const INICIO_FOCO: &[u8] = b"\x1b[?1004h";
const FIM_FOCO: &[u8] = b"\x1b[?1004l";

struct AplicativoNoTerminal {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    entrada: Box<dyn Write + Send>,
    saida: Arc<Mutex<Vec<u8>>>,
    leitor: thread::JoinHandle<()>,
}

impl AplicativoNoTerminal {
    fn iniciar(home: &Path) -> Self {
        let par = NativePtySystem::default()
            .openpty(PtySize {
                rows: 28,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("criar pseudo-terminal");
        let mut comando = CommandBuilder::new(env!("CARGO_BIN_EXE_tuipe"));
        comando.env("HOME", home);
        comando.env("XDG_CONFIG_HOME", home.join("config"));
        comando.env("XDG_DATA_HOME", home.join("data"));
        comando.env("TERM", "xterm-256color");
        comando.env("TUIPE_ICONS", "unicode");
        comando.env("TUIPE_COLORS", "256");

        let child = par
            .slave
            .spawn_command(comando)
            .expect("iniciar tuipe no pseudo-terminal");
        drop(par.slave);

        let mut reader = par.master.try_clone_reader().expect("clonar saída da PTY");
        let entrada = par.master.take_writer().expect("abrir entrada da PTY");
        let saida = Arc::new(Mutex::new(Vec::new()));
        let saida_do_leitor = Arc::clone(&saida);
        let leitor = thread::spawn(move || {
            let mut bloco = [0_u8; 4_096];
            while let Ok(quantidade) = reader.read(&mut bloco) {
                if quantidade == 0 {
                    break;
                }
                saida_do_leitor
                    .lock()
                    .expect("saída desbloqueada")
                    .extend_from_slice(&bloco[..quantidade]);
            }
        });

        Self {
            child,
            master: par.master,
            entrada,
            saida,
            leitor,
        }
    }

    fn escrever(&mut self, bytes: &[u8]) {
        self.entrada.write_all(bytes).expect("enviar tecla");
        self.entrada.flush().expect("descarregar tecla");
    }

    fn esperar_saida(&self, trecho: &[u8]) {
        let limite = Instant::now() + Duration::from_secs(5);
        while Instant::now() < limite {
            if contem(&self.saida.lock().expect("saída desbloqueada"), trecho) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "a sequência {:?} não apareceu na saída: {}",
            String::from_utf8_lossy(trecho),
            String::from_utf8_lossy(&self.saida.lock().expect("saída desbloqueada"))
        );
    }

    fn esperar_encerrar(mut self) -> Vec<u8> {
        let limite = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("consultar processo") {
                break status;
            }
            if Instant::now() >= limite {
                let _ = self.child.kill();
                panic!("tuipe não encerrou em cinco segundos");
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(status.success(), "tuipe encerrou com {status:?}");
        drop(self.entrada);
        drop(self.master);
        self.leitor.join().expect("aguardar leitor da PTY");
        Arc::try_unwrap(self.saida)
            .expect("a saída ainda tem outro proprietário")
            .into_inner()
            .expect("saída desbloqueada")
    }
}

fn contem(conteudo: &[u8], trecho: &[u8]) -> bool {
    conteudo
        .windows(trecho.len())
        .any(|janela| janela == trecho)
}

fn confirmar_protocolos_restaurados(saida: &[u8]) {
    for sequencia in [FIM_COLAGEM, FIM_FOCO, b"\x1b[?1000l", b"\x1b[?1006l"] {
        assert!(
            contem(saida, sequencia),
            "protocolo não restaurado: {:?}",
            String::from_utf8_lossy(sequencia)
        );
    }
}

#[test]
fn devolve_protocolos_ao_terminal_ao_sair() {
    let home = tempfile::tempdir().expect("criar diretório temporário");
    let mut app = AplicativoNoTerminal::iniciar(home.path());
    app.esperar_saida(INICIO_COLAGEM);
    app.esperar_saida(INICIO_FOCO);

    app.escrever(b"\x1b");
    thread::sleep(Duration::from_millis(80));
    app.escrever(b"q");

    confirmar_protocolos_restaurados(&app.esperar_encerrar());
}

#[test]
fn salva_sessao_e_restaura_terminal_ao_receber_sigterm() {
    let home = tempfile::tempdir().expect("criar diretório temporário");
    let mut app = AplicativoNoTerminal::iniciar(home.path());
    app.esperar_saida(INICIO_COLAGEM);
    app.escrever(b"a");
    let pid = app.child.process_id().expect("processo sem PID");

    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("enviar SIGTERM");
    assert!(status.success(), "kill não conseguiu enviar SIGTERM");

    confirmar_protocolos_restaurados(&app.esperar_encerrar());
    let banco = home.path().join("data/tuipe/tuipe.db");
    assert!(banco.exists(), "a sessão interrompida não criou o banco");
    let conexao = rusqlite::Connection::open(banco).expect("abrir banco da sessão interrompida");
    let sessoes_com_eventos: u64 = conexao
        .query_row("SELECT COUNT(*) FROM raw_events", [], |linha| linha.get(0))
        .expect("contar sessões interrompidas");
    assert_eq!(sessoes_com_eventos, 1);
}
