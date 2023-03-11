use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use futures_util::future::{select, Either};
use futures_util::stream::{StreamExt, SplitSink, SplitStream};
use futures_util::{SinkExt, pin_mut};
use regex::Regex;
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Message, error::Error};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

pub type RespProps = (&'static[&'static str], Option<serde_json::Value>);

static MOO_COUNT: AtomicUsize = AtomicUsize::new(0);

pub struct Moo;

#[derive(Debug)]
pub struct MooSender {
    pub moo_id: usize,
    pub req_id: usize,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>
}

pub struct MooReceiver {
    pub moo_id: usize,
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>
}

impl Moo {
    pub async fn new(ip: &IpAddr, port: &str) -> Result<(MooSender, MooReceiver), Error> {
        let moo_id = MOO_COUNT.fetch_add(1, Ordering::Relaxed);
        let url = Url::parse(&format!("ws://{}:{}/api", ip, port)).unwrap();

        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        let (write, read) = ws.split();

        let sender = MooSender {
            moo_id,
            write,
            req_id: 0
        };

        let receiver = MooReceiver {
            moo_id,
            read
        };

        Ok((sender, receiver))
    }
}

impl MooSender {
    pub async fn send_msg(&mut self, request_id: usize, props: &RespProps) -> Result<usize, Error> {
        let (hdr, body) = props;
        if hdr.len() > 1 {
            let action = hdr[0];
            let state = hdr[1];
            let mut header = format!("MOO/1 {} {}\nRequest-Id: {}\n", action, state, request_id);
    
            if let Some(body) = &body {
                let body = body.to_string();
    
                println!("-> {} {} {} {}", action, request_id, state, body);
    
                let body_len = body.as_bytes().len();
    
                header.push_str(format!("Content-Length: {}\nContent-Type: application/json\n\n", body_len).as_str());
                header.push_str(&body);
            } else {
                println!("-> {} {} {}", action, request_id, state);
    
                header.push('\n');
            }
    
            self.write.send(Message::Binary(Vec::from(header))).await?;

            if action == "REQUEST" {
                self.req_id += 1;
            }
        }

        Ok(self.moo_id)
    }
}

impl MooReceiver {
    pub async fn receive_response(&mut self) -> Result<serde_json::Value, Error> {
        let (ws_tx, mut ws_rx) = mpsc::channel::<Result<serde_json::Value, Error>>(4);

        let tx = self.read.by_ref().for_each(|message| async {
            match message {
                Ok(message) => {
                    if let Ok(data) = message.into_text() {
                        if let Some(json) = Self::parse(&data) {
                            ws_tx.send(Ok(json)).await.unwrap();
                        }
                    }
                }
                Err(err) => {
                    ws_tx.send(Err(err)).await.unwrap();
                }
            }
        });
        let rx = ws_rx.recv();

        pin_mut!(tx, rx);

        match select(tx, rx).await {
            Either::Right((Some(response), _)) => {
                if let Ok(msg) = &response {
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
                }

                response
            }
            Either::Right((None, _)) => Err(Error::ConnectionClosed),
            Either::Left((_, _)) => Err(Error::ConnectionClosed)
        }
    }

    pub fn clean_up(&self) {

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
