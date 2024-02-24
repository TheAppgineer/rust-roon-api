use std::io::Read;
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

#[macro_export]
macro_rules! send_continue {
    ($name:literal, $body:expr) => {
        vec![(&["CONTINUE", $name], $body)]
    };
    ($resp_props:ident, $name:literal, $body:expr) => {
        $resp_props.push((&["CONTINUE", $name], $body))
    };
}

#[macro_export]
macro_rules! send_complete {
    ($name:literal, $body:expr) => {
        vec![(&["COMPLETE", $name], $body)]
    };
    ($resp_props:ident, $name:literal, $body:expr) => {
        $resp_props.push((&["COMPLETE", $name], $body))
    };
}

#[macro_export]
macro_rules! send_continue_all {
    ($sub_type:literal, $name:literal, $body:expr) => {
        vec![(&[$sub_type, "CONTINUE", $name], $body)]
    };
    ($resp_props:ident, $sub_type:literal, $name:literal, $body:expr) => {
        $resp_props.push((&[$sub_type, "CONTINUE", $name], $body))
    };
}

#[macro_export]
macro_rules! send_complete_all {
    ($sub_type:literal, $name:literal, $body:expr) => {
        vec![(&[$sub_type, "COMPLETE", $name], $body)]
    };
    ($resp_props:ident, $sub_type:literal, $name:literal, $body:expr) => {
        $resp_props.push((&[$sub_type, "COMPLETE", $name], $body))
    };
}

#[derive(Clone, Debug)]
pub enum ContentType {
    Void,
    Json,
    Jpeg(Vec<u8>),
    Png(Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct Moo {
    pub id: usize,
    pub req_id: Arc<Mutex<usize>>,
    msg_tx: Sender<(usize, String)>,
    sub_key: Arc<Mutex<usize>>,
}

#[derive(Debug)]
pub struct MooSender {
    pub id: usize,
    pub req_id: Arc<Mutex<usize>>,
    pub msg_rx: Receiver<(usize, String)>,
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    quiet_reqs: Arc<Mutex<Vec<usize>>>
}

#[derive(Debug)]
pub struct MooReceiver {
    pub id: usize,
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    quiet_reqs: Arc<Mutex<Vec<usize>>>
}

impl Moo {
    pub async fn new(ip: &IpAddr, port: &str) -> Result<(Moo, MooSender, MooReceiver), Error> {
        let id = MOO_COUNT.fetch_add(1, Ordering::Relaxed);
        let url = Url::parse(&format!("ws://{}:{}/api", ip, port)).unwrap();
        let req_id = Arc::new(Mutex::new(0));
        let quiet_reqs = Arc::new(Mutex::new(Vec::new()));

        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        let (write, read) = ws.split();
        let (msg_tx, msg_rx) = mpsc::channel::<(usize, String)>(10);

        let req_id_clone = req_id.clone();
        let moo = Moo {
            id,
            req_id: req_id_clone,
            msg_tx,
            sub_key: Arc::new(Mutex::new(0)),
        };

        let sender = MooSender {
            id,
            req_id,
            msg_rx,
            write,
            quiet_reqs: quiet_reqs.clone(),
        };

        let receiver = MooReceiver {
            id,
            read,
            quiet_reqs,
        };

        Ok((moo, sender, receiver))
    }

    pub async fn send_req(&self, name: String, body: Option<serde_json::Value>) -> Result<usize, SendError<(usize, String)>> {
        let req_id = self.req_id.lock().unwrap().to_owned();
        let (msg_string, log_string) = Moo::create_msg_string(req_id, &["REQUEST", &name], body.as_ref());

        self.msg_tx.send((req_id, msg_string)).await?;

        log::info!("{}", log_string);

        let mut next_req_id = self.req_id.lock().unwrap();
        *next_req_id += 1;

        Ok(req_id)
    }

    pub async fn send_sub_req(&self, svc_name: &str, req_name: &str, args: Option<serde_json::Value>) -> Result<(usize, usize), SendError<(usize, String)>> {
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

        match self.send_req(name, body).await {
            Ok(req_id) => Ok((req_id, sub_key)),
            Err(err) => Err(err)
        }
    }

    pub async fn send_unsub_req(&self, svc_name: &str, req_name: &str, sub_key: usize) -> Result<usize, SendError<(usize, String)>> {
        let name = format!("{}/unsubscribe_{}", svc_name, req_name);
        let body = Some(json!({"subscription_key": sub_key}));

        self.send_req(name, body).await
    }

    pub async fn send_msg_string(&self, req_id: usize, msg_string: (String, String)) -> Result<(), SendError<(usize, String)>> {
        self.msg_tx.send((req_id, msg_string.0)).await?;

        log::info!("{}", msg_string.1);

        Ok(())
    }

    pub fn create_msg_string(req_id: usize, hdr: &[&str], body: Option<&serde_json::Value>) -> (String, String) {
        let action = hdr[0];
        let state = hdr[1];
        let mut msg_string = format!("MOO/1 {} {}\nRequest-Id: {}\n", action, state, req_id);
        let mut log_string = format!("-> {} {} {}", action, req_id, state);

        if let Some(body) = body {
            let body = body.to_string();

            log_string.push_str(format!(" {}", body).as_str());

            let body_len = body.as_bytes().len();

            msg_string.push_str(format!("Content-Length: {}\nContent-Type: application/json\n\n", body_len).as_str());
            msg_string.push_str(&body);
        } else {
            msg_string.push('\n');
        }

        (msg_string, log_string)
    }
}

impl MooSender {
    pub async fn send_req(&mut self, name: &str, body: Option<&serde_json::Value>) -> Result<(), Error> {
        let req_id = self.req_id.lock().unwrap().to_owned();

        self.send_msg(req_id, &["REQUEST", name], body).await
    }

    pub async fn send_msg(&mut self, req_id: usize, hdr: &[&str], body: Option<&serde_json::Value>) -> Result<(), Error> {
        if hdr.len() > 1 {
            let (msg_string, log_string) = Moo::create_msg_string(req_id, hdr, body);

            self.write.send(Message::Binary(Vec::from(msg_string))).await?;

            let mut quiet_reqs = self.quiet_reqs.lock().unwrap();
            let quiet = quiet_reqs.iter().position(|id| *id == req_id);

            if quiet.is_none() {
                log::info!("{}", log_string);
            } else {
                log::debug!("{}", log_string);
            }

            let verb = hdr[0];

            if verb == "REQUEST" {
                let mut req_id = self.req_id.lock().unwrap();
                *req_id += 1;
            } else if verb == "COMPLETE" {
                if let Some(index) = quiet {
                    quiet_reqs.remove(index);
                }
            }
        }

        Ok(())
    }

    pub async fn send_msg_string(&mut self, msg_string: &str) -> Result<(), Error> {
        self.write.send(Message::Binary(Vec::from(msg_string))).await?;

        Ok(())
    }
}

impl MooReceiver {
    pub async fn receive_response(&mut self) -> Result<(serde_json::Value, ContentType), Error> {
        let timeout = std::time::Duration::from_secs(10);
        let result = tokio::time::timeout(timeout, self.read.next())
            .await
            .map_err(|_| Error::ConnectionClosed)?;

        match result {
            Some(msg) => {
                let msg = msg?;

                if let Message::Binary(data) = &msg {
                    if let Some((hdr, body)) = Self::parse(data) {
                        let req_id = hdr["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                        let quiet = hdr["headers"]["Logging"] == "quiet";
                        let header = if hdr["verb"] == "REQUEST" {
                            format!("<- {} {} {}/{}", hdr["verb"].as_str().unwrap(),
                                                      req_id,
                                                      hdr["service"].as_str().unwrap(),
                                                      hdr["name"].as_str().unwrap())
                        } else {
                            format!("<- {} {} {}", hdr["verb"].as_str().unwrap(),
                                                   req_id,
                                                   hdr["name"].as_str().unwrap())
                        };
                        let msg_string = match &body {
                            ContentType::Json =>
                                format!("{} {}", header, hdr["body"]),
                            ContentType::Jpeg(_) =>
                                format!("{} <jpeg>", header),
                            ContentType::Png(_) =>
                                format!("{} <png>", header),
                            ContentType::Void =>
                                header,
                        };

                        let mut quiet_reqs = self.quiet_reqs.lock().unwrap();

                        quiet_reqs.push(req_id);

                        if quiet {
                            log::debug!("{}", msg_string);
                        } else {
                            log::info!("{}", msg_string);
                        }

                        return Ok((hdr, body))
                    }
                }
                Err(Error::ConnectionClosed)
            }
            None => {
                Err(Error::ConnectionClosed)
            }
        }
    }

    fn parse(data: &[u8]) -> Option<(serde_json::Value, ContentType)> {
        enum State {
            Id,
            Header,
        }
        let mut state = State::Id;
        let mut header = Ok(String::new());
        let mut body = Vec::new();

        for (index, byte) in data.bytes().enumerate() {
            let byte = byte.ok()?;
            if byte == b'\n' && *data.get(index + 1)? == b'\n' {
                let bytes = Vec::from(&data[0..index]);

                header = String::from_utf8(bytes);
                body = Vec::from(&data[index + 2..]);

                break;
            }
        }

        let hdr_string = header.ok()?;
        let mut msg = json!({});

        for line in hdr_string.split('\n') {
            match &state {
                State::Id => {
                    let line_regex = Regex::new(r"^MOO/([0-9]+) ([A-Z]+) (.*)").unwrap();
                    let matches = line_regex.captures(line)?;
                    let verb = matches.get(2)?.as_str();

                    if verb == "REQUEST" {
                        let request_regex = Regex::new(r"([^/]+)/(.*)").unwrap();
                        let request = matches.get(3)?.as_str();
                        let matches = request_regex.captures(request)?;

                        msg["service"] = matches.get(1)?.as_str().into();
                        msg["name"] = matches.get(2)?.as_str().into();
                    } else {
                        msg["name"] = matches.get(3)?.as_str().into();
                    }

                    msg["verb"] = verb.into();
                    state = State::Header;
                }
                State::Header => {
                    if !line.is_empty() {
                        let line_regex = Regex::new(r"([^:]+): *(.*)").unwrap();
                        let matches = line_regex.captures(line)?;
                        let header = matches.get(1)?.as_str();
                        let value = matches.get(2)?.as_str();

                        if header == "Content-Type" {
                            msg["content_type"] = value.into();
                        } else if header == "Content-Length" {
                            msg["content_length"] = value.parse::<usize>().unwrap().into();
                        } else if header == "Request-Id" {
                            msg["request_id"] = value.into();
                        } else {
                            msg["headers"][header] = value.into();
                        }
                    }
                }
            }
        }

        if msg["content_type"] == "application/json" {
            let body = String::from_utf8(body);

            msg["body"] = serde_json::from_str(&body.ok()?).unwrap();

            Some((msg, ContentType::Json))
        } else if msg["content_type"] == "image/jpeg" {
            Some((msg, ContentType::Jpeg(body)))
        } else if msg["content_type"] == "image/png" {
            Some((msg, ContentType::Png(body)))
        } else {
            Some((msg, ContentType::Void))
        }
    }
}
