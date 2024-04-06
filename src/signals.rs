use std::error::Error;
use tokio::{
    select,
    signal::unix::{signal, Signal, SignalKind},
    sync::watch,
};
use tracing::{debug, error, info};

pub type ShutdownReceiver = watch::Receiver<bool>;
pub type ShutdownSender = watch::Sender<bool>;

#[derive(Debug)]
pub struct SignalHandler {
    terminate: Signal,
    interrupt: Signal,
    quit: Signal,
    hangup: Signal,

    shutdown_channel_tx: ShutdownSender,
    shutdown_channel_rx: ShutdownReceiver,
}

impl SignalHandler {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let (shutdown_channel_tx, shutdown_channel_rx) = watch::channel(false);
        let receiver = Self {
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
            quit: signal(SignalKind::quit())?,
            hangup: signal(SignalKind::hangup())?,
            shutdown_channel_tx,
            shutdown_channel_rx,
        };

        Ok(receiver)
    }

    pub fn get_rx_channel(&self) -> ShutdownReceiver {
        self.shutdown_channel_rx.clone()
    }

    pub async fn shutdown_on_signal(&mut self) {
        debug!("Signal handler has been installed");
        let signal = self.wait_for_signal().await;

        info!("{signal} signal has been received, shutting down");
        if let Err(e) = self.shutdown_channel_tx.send(true) {
            error!("can't send shutdown message: {}", e);
        } else {
            debug!("shutdown message has been sent");
        }
    }

    async fn wait_for_signal(&mut self) -> &str {
        select! {
            _ = self.terminate.recv() => "TERM",
            _ = self.interrupt.recv() => "INT",
            _ = self.quit.recv() => "QUIT",
            _ = self.hangup.recv() => "HANGUP",
        }
    }
}
