use serde::Deserialize;
use std::time::Duration;
use systemd::{journal, Journal};
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    sync::broadcast::{self, Receiver, Sender},
};
use tokio_util::io::ReaderStream;

pub enum MinecraftError {
    LogError(tokio::io::Error),
    CommandError(String),
}

impl From<tokio::io::Error> for MinecraftError {
    fn from(e: tokio::io::Error) -> Self {
        MinecraftError::LogError(e)
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct MinecraftConfig {
    log_path: Option<String>,
    socket_path: Option<String>,
    systemd_unit: Option<String>,
}

#[derive(Clone)]
pub struct MinecraftControl {
    config: MinecraftConfig,
    tx: Sender<String>,
}

pub fn init(config: Option<MinecraftConfig>) -> MinecraftControl {
    let mc_config = match config {
        Some(c) => c,
        None => MinecraftConfig {
            log_path: None,
            socket_path: None,
            systemd_unit: None,
        },
    };
    let (tx, _): (Sender<String>, Receiver<String>) = broadcast::channel(16);
    let tx_real = tx.clone();
    let systemd_unit: String = match mc_config.systemd_unit {
        Some(ref s) => s.clone(),
        None => String::from("minecraft-server.service"),
    };

    let _ = tokio::task::spawn_blocking(move || read_journal(tx_real, systemd_unit));

    MinecraftControl {
        config: mc_config,
        tx,
    }
}

impl MinecraftControl {
    pub fn subscribe(&mut self) -> Receiver<String> {
        self.tx.subscribe()
    }

    pub async fn log(&self) -> Result<ReaderStream<tokio::fs::File>, MinecraftError> {
        let filename = match &self.config.log_path {
            Some(f) => f,
            None => &String::from("/var/lib/minecraft/logs/latest.log"),
        };
        let file = tokio::fs::File::open(filename).await?;
        Ok(ReaderStream::new(file))
    }

    pub async fn command(&self, mut command: String) -> Result<bool, MinecraftError> {
        let filename = match &self.config.socket_path {
            Some(f) => f,
            None => &String::from("/run/minecraft-server.stdin"),
        };
        let mut file = OpenOptions::new()
            .read(false)
            .write(true)
            .open(filename)
            .await
            .unwrap();
        if !command.ends_with("\n") {
            command = format!("{}\n", command);
        }
        let bytes = command.as_bytes();
        let _ = file.write_all(bytes).await.unwrap();
        let _ = file.flush().await.unwrap();

        Ok(true)
    }
}

fn read_journal(tx: Sender<String>, systemd_unit: String) {
    println!("opening journal");
    let _ = tx.send("starting up".to_owned());
    let mut j: Journal = journal::OpenOptions::default().open().unwrap();
    let _ = j.seek_tail();
    let _ = j.previous();

    while let Ok(e) = j.next_entry() {
        match e {
            Some(entry) => {
                let unit = match entry.get("_SYSTEMD_UNIT") {
                    Some(value) => value,
                    None => &"".to_owned(),
                };
                if unit == &systemd_unit {
                    let message = match entry.get("MESSAGE") {
                        Some(value) => value,
                        None => &"".to_owned(),
                    };

                    match tx.send(message.to_owned()) {
                        Ok(_s) => {}
                        Err(e) => println!("could not write to tx {}", e),
                    };
                }
            }
            None => {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}
