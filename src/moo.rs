use std::net::IpAddr;
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use futures_util::stream::{StreamExt, SplitSink, SplitStream};
use futures_util::SinkExt;
use regex::Regex;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, Receiver, Sender, error::SendError};
use tokio_tungstenite::tungstenite::{Message, error::Error};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

static MOO_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
pub struct Moo {
    pub id: usize,
    pub req_id: Arc<Mutex<usize>>,
    msg_tx: Sender<String>,
    sub_key: Arc<Mutex<usize>>
}

#[derive(Debug)]
pub struct MooSender {
    pub id: usize,
    pub req_id: Arc<Mutex<usize>>,
    pub msg_rx: Receiver<String>,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>
}

pub struct MooReceiver {
    pub id: usize,
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>
}

impl Moo {
    pub async fn new(ip: &IpAddr, port: &str) -> Result<(Moo, MooSender, MooReceiver), Error> {
        let id = MOO_COUNT.fetch_add(1, Ordering::Relaxed);
        let url = Url::parse(&format!("ws://{}:{}/api", ip, port)).unwrap();
        let req_id = Arc::new(Mutex::new(0));

        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        let (write, read) = ws.split();
        let (msg_tx, msg_rx) = mpsc::channel::<String>(4);

        let req_id_clone = req_id.clone();
        let moo = Moo {
            id,
            req_id: req_id_clone,
            msg_tx,
            sub_key: Arc::new(Mutex::new(0))
        };

        let sender = MooSender {
            id,
            req_id,
            msg_rx,
            write
        };

        let receiver = MooReceiver {
            id,
            read
        };

        Ok((moo, sender, receiver))
    }

    pub async fn send_req(&self, name: String, body: Option<serde_json::Value>) -> Result<usize, SendError<String>> {
        let req_id = self.req_id.lock().unwrap().to_owned();
        let msg_string = Moo::create_msg_string(req_id, &["REQUEST", &name], body.as_ref());

        self.msg_tx.send(msg_string).await?;

        Ok(req_id)
    }

    pub async fn send_sub_req(&self, svc_name: &str, req_name: &str, args: Option<serde_json::Value>) -> Result<usize, SendError<String>> {
        let body;

        let mut sub_key = *self.sub_key.lock().unwrap();
        sub_key += 1;

        if let Some(mut args) = args {
            args["subscription_key"] = sub_key.into();
            body = Some(args);
        } else {
            body = Some(json!({"subscription_key": sub_key}));
        }

        let name = format!("{}/subscribe_{}", svc_name, req_name);

        self.send_req(name, body).await
    }

    pub async fn send_msg_string(&self, msg_string: String) -> Result<(), SendError<String>> {
        self.msg_tx.send(msg_string).await
    }

    pub fn create_msg_string(request_id: usize, hdr: &[&str], body: Option<&serde_json::Value>) -> String {
        let action = hdr[0];
        let state = hdr[1];
        let mut msg_string = format!("MOO/1 {} {}\nRequest-Id: {}\n", action, state, request_id);

        if let Some(body) = body {
            let body = body.to_string();

            println!("-> {} {} {} {}", action, request_id, state, body);

            let body_len = body.as_bytes().len();

            msg_string.push_str(format!("Content-Length: {}\nContent-Type: application/json\n\n", body_len).as_str());
            msg_string.push_str(&body);
        } else {
            println!("-> {} {} {}", action, request_id, state);

            msg_string.push('\n');
        }

        msg_string
    }
}

impl MooSender {
    pub async fn send_req(&mut self, name: &str, body: Option<&serde_json::Value>) -> Result<usize, Error> {
        let req_id = self.req_id.lock().unwrap().to_owned();

        self.send_msg(req_id, &["REQUEST", name], body).await
    }

    pub async fn send_msg(&mut self, request_id: usize, hdr: &[&str], body: Option<&serde_json::Value>) -> Result<usize, Error> {
        if hdr.len() > 1 {
            let action = hdr[0];
            let msg_string = Moo::create_msg_string(request_id, hdr, body);

            self.write.send(Message::Binary(Vec::from(msg_string))).await?;

            if action == "REQUEST" {
                let mut req_id = self.req_id.lock().unwrap();
                *req_id += 1;
            }
        }

        Ok(self.id)
    }

    pub async fn send_msg_string(&mut self, msg_string: &str) -> Result<usize, Error> {
        if msg_string.contains("REQUEST") {
            let mut req_id = self.req_id.lock().unwrap();
            *req_id += 1;
        }

        self.write.send(Message::Binary(Vec::from(msg_string))).await?;

        Ok(self.id)
    }
}

impl MooReceiver {
    pub async fn receive_response(&mut self) -> Result<serde_json::Value, Error> {
        match self.read.next().await {
            Some(msg) => {
                let msg = msg?;

                if let Ok(data) = msg.into_text() {
                    if let Some(msg) = Self::parse(&data) {
                        if msg["verb"] == "REQUEST" {
                            print!("<- {} {} {}/{}", msg["verb"].as_str().unwrap(),
                                                     msg["request_id"].as_str().unwrap(),
                                                     msg["service"].as_str().unwrap(),
                                                     msg["name"].as_str().unwrap());
                        } else {
                            print!("<- {} {} {}", msg["verb"].as_str().unwrap(),
                                                  msg["request_id"].as_str().unwrap(),
                                                  msg["name"].as_str().unwrap());
                        }
    
                        if msg["content_length"].is_null() {
                            println!("");
                        } else {
                            println!(" {}", msg["body"].to_string());
                        }

                        return Ok(msg)
                    }
                }
                Err(Error::ConnectionClosed)
            }
            None => Err(Error::ConnectionClosed)
        }
    }

    fn parse(data: &str) -> Option<serde_json::Value> {
        enum State {
            Id,
            Header,
            Body
        }
        let mut state = State::Id;
        let mut json = json!({
            "headers": {}
        });

        for line in data.split('\n') {
            match &state {
                State::Id => {
                    let line_regex = Regex::new(r"^MOO/([0-9]+) ([A-Z]+) (.*)").unwrap();
                    let matches = line_regex.captures(line)?;
                    let verb = matches.get(2)?.as_str();

                    if verb == "REQUEST" {
                        let request_regex = Regex::new(r"([^/]+)/(.*)").unwrap();
                        let request = matches.get(3)?.as_str();
                        let matches = request_regex.captures(request)?;
            
                        json["service"] = matches.get(1)?.as_str().into();
                        json["name"] = matches.get(2)?.as_str().into();
                    } else {
                        json["name"] = matches.get(3)?.as_str().into();
                    }

                    json["verb"] = verb.into();
                    state = State::Header;
                }
                State::Header => {
                    if line.len() > 0 {
                        let line_regex = Regex::new(r"([^:]+): *(.*)").unwrap();
                        let matches = line_regex.captures(line)?;
                        let header = matches.get(1)?.as_str();
                        let value = matches.get(2)?.as_str();

                        if header == "Content-Type" {
                            json["content_type"] = value.into();
                        } else if header == "Content-Length" {
                            json["content_length"] = value.parse::<usize>().unwrap().into();
                        } else if header == "Request-Id" {
                            json["request_id"] = value.into();
                        } else {
                            json["headers"][header] = value.into();
                        }
                    } else {
                        state = State::Body;
                    }
                }
                State::Body => {
                    if json["content_type"] == "application/json" {
                        json["body"] = serde_json::from_str(line).unwrap();
                    }
                }
            }
        }

        match state {
            State::Body => return Some(json),
            _ => return None
        }
    }
}
