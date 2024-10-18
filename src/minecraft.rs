use std::time::Duration;
use systemd::{journal, Journal};
use tokio::sync::broadcast::Sender;

pub struct MinecraftControl {
    tx: Sender<String>,
}

pub fn new(tx: Sender<String>) -> MinecraftControl {
    MinecraftControl { tx }
}

impl MinecraftControl {
    pub fn read_journal(&mut self) {
        println!("opening journal");
        let _ = self.tx.send("starting up".to_owned());
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
                    if unit == "minecraft-server.service" {
                        let message = match entry.get("MESSAGE") {
                            Some(value) => value,
                            None => &"".to_owned(),
                        };

                        match self.tx.send(message.to_owned()) {
                            Ok(_s) => { }
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
}
