use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use clap::Parser;
use clipboard_rs::{Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext};
use rumqttc::{Client, Event, MqttOptions, QoS, TlsConfiguration, Transport};
use std::sync::mpsc;
use log::{error, info};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    device: String,

    #[arg(short, long)]
    user: String,

    #[arg(short, long)]
    cert_dir: String,

    #[arg(short, long)]
    server: String,

    #[arg(short, long, default_value = "8883")]
    port: u16,
}

struct Manager {
    publish_sender: mpsc::Sender<String>,
    ctx: Arc<Mutex<ClipboardContext>>,
    current_content: String,
}

impl Manager {
    fn new(ctx: Arc<Mutex<ClipboardContext>>, publish_sender: mpsc::Sender<String>) -> Manager {
        Manager {
            ctx,
            publish_sender,
            current_content: String::new(),
        }
    }
}

impl ClipboardHandler for Manager {
    fn on_clipboard_change(&mut self) {
        let ctx = self.ctx.lock().unwrap();

        if let Ok(text) = ctx.get_text() {
            if text != self.current_content {
                self.current_content = text;
                if let Err(e) = self.publish_sender.send(self.current_content.clone()) {
                    error!("Error sending message: {}", e);
                }
            }
        }
    }
}

fn main() {
    env_logger::init();

    let args = Args::parse();
    let ca_cert_path = Path::new(&args.cert_dir).join("ca.crt");
    let cert_prefix = format!("{}-{}", args.user, args.device);
    let cert_path = Path::new(&args.cert_dir).join(format!("{cert_prefix}.crt"));
    let key_path = Path::new(&args.cert_dir).join(format!("{cert_prefix}.key"));


    let mut ca_file = std::fs::File::open(&ca_cert_path).unwrap();
    let mut ca_bytes = Vec::new();
    ca_file.read_to_end(&mut ca_bytes).unwrap();

    let mut cert_file = std::fs::File::open(&cert_path).unwrap();
    let mut cert_bytes = Vec::new();
    cert_file.read_to_end(&mut cert_bytes).unwrap();

    let mut key_file = std::fs::File::open(&key_path).unwrap();
    let mut key_bytes = Vec::new();
    key_file.read_to_end(&mut key_bytes).unwrap();


    let (publish_sender, publish_receiver) = mpsc::channel();
    let ctx = Arc::new(Mutex::new(ClipboardContext::new().unwrap()));

    let manager = Manager::new(ctx.clone(), publish_sender);
    let mut watcher = ClipboardWatcherContext::new().unwrap();
    let shutdown_channel = watcher.add_handler(manager).get_shutdown_channel();

    std::thread::spawn(move || {
        watcher.start_watch();
    });

    let transport = Transport::Tls(TlsConfiguration::Simple {
        ca: ca_bytes,
        alpn: None,
        client_auth: Some((cert_bytes, key_bytes)),
    });

    let mut mqtt_opt = MqttOptions::new(args.device, args.server, args.port);
    mqtt_opt.set_keep_alive(Duration::from_secs(5));
    mqtt_opt.set_transport(transport);

    let (client, mut connection) = Client::new(mqtt_opt, 10);

    let topic = format!("clipboard/{}", args.user);
    client.subscribe(topic.clone(), QoS::AtMostOnce).unwrap();
    info!("subscribed {}", topic.clone());

    std::thread::spawn(move || {
        while let Ok(content) = publish_receiver.recv() {
            let content_len = content.len();
            if let Err(e) = client.publish(topic.clone(), QoS::AtLeastOnce, false, content) {
                error!("Failed to publish message: {:?}", e);
                break;
            } else {
                info!("publish {} bytes to cloud", content_len);
            }
        }
    });


    for (_, notification) in connection.iter().enumerate() {
        match notification {
            Ok(Event::Incoming(rumqttc::Incoming::Publish(publish))) => {
                if let Ok(content) = String::from_utf8(publish.payload.to_vec()) {
                    info!("get {} bytes from cloud", content.len());
                    let ctx = ctx.lock().unwrap();
                    if let Err(e) = ctx.set_text(content) {
                        error!("Failed to set clipboard content: {:?}", e);
                    }
                }
            }
            Err(err) => {
                error!("Failed to receive notification: {:?}", err);
            }
            _ => {}
        }
    }

    shutdown_channel.stop();
    info!("exit");
}
