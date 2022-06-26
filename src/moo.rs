use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::{AtomicU32, Ordering}};

use json::{object, JsonValue};
use regex::Regex;
use tungstenite::Error;

use crate::transport_websocket::Transport;

static MOO_COUNT: AtomicU32 = AtomicU32::new(0);

pub struct Core {
    pub core_id: String,
    pub display_name: String,
    pub display_version: String
}

pub struct Moo {
    pub mooid: u32,
    pub transport: Arc<Mutex<Transport>>,
    pub unique_id: String,
    pub core: Option<Core>,
    req_id: u32,
    requests: HashMap<u32, Box<dyn Fn(&mut Moo, Option<&JsonValue>) + Send + 'static>>
}

impl Moo {
    pub fn new(transport: Transport, unique_id: String) -> Self {
        let mooid = MOO_COUNT.fetch_add(1, Ordering::Relaxed);

        Self {
            mooid,
            transport: Arc::new(Mutex::new(transport)),
            core: None,
            unique_id,
            req_id: 0,
            requests: HashMap::new()
        }
    }

    pub fn send_request<F>(&mut self, name: &str, body: Option<&JsonValue>, cb: F) -> Result<(), Error>
    where F: Fn(&mut Moo, Option<&JsonValue>) + Send + 'static
    {
        let req_id = self.req_id;
        let mut header = format!("MOO/1 REQUEST {}\nRequest-Id: {}\n", name, req_id);
        let mut transport = self.transport.lock().unwrap();

        if let Some(body) = body {
            let body = json::stringify(body.clone());

            println!("-> REQUEST {} {} {}", req_id, name, body);

            let body = body.as_bytes();

            header.push_str(format!("Content-Length: {}\nContent-Type: application/json\n\n", body.len()).as_str());

            transport.send(&[header.as_bytes(), body].concat())?;
        } else {
            println!("-> REQUEST {} {}", req_id, name);

            header.push('\n');

            transport.send(header.as_bytes())?;
        }

        self.req_id += 1;
        self.requests.insert(req_id, Box::new(cb));

        Ok(())
    }

    pub fn receive_response(&mut self) -> Result<JsonValue, Box<dyn std::error::Error>> {
        let buf = self.transport.lock().unwrap().receive()?;
        let mut s: usize = 0;
        let mut e: usize = 0;
        let mut state = "";
        let mut msg = object! {
            headers: {}
        };

        while e < buf.len() {
            if buf[e] == 0x0a {
                if state == "header" {
                    if s == e {
                        if msg["request_id"].is_null() {
                            return Err("MOO: missing Request-Id header")?;
                        }

                        let request_id = msg["request_id"].as_str().unwrap().parse::<u32>()?;

                        if msg["content_length"].is_null() {
                            if !msg["content_type"].is_null() {
                                return Err("MOO: bad message; has Content-Type but not Content-Length")?;
                            }
                            if e != buf.len() - 1 {
                                return Err("MOO: bad message; has no Content-Length, but data after headers")?;
                            }

                            if msg["verb"] == "REQUEST" {
                                println!("<- {} {} {}/{}", msg["verb"], request_id, msg["service"], msg["name"]);
                            } else {
                                println!("<- {} {} {}", msg["verb"], request_id, msg["name"]);
                            }
                        } else {
                            let content_length = msg["content_length"].as_usize().unwrap();

                            if content_length > 0 {
                                if msg["content_type"].is_null() {
                                    return Err("MOO: bad message; has Content-Length but not Content-Type")?;
                                } else {
                                    let body = &buf[e+1..e+1+content_length];

                                    if msg["content_type"] == "application/json" {
                                        let body = std::str::from_utf8(body)?;

                                        msg["body"] = json::parse(body)?;

                                        if msg["verb"] == "REQUEST" {
                                            println!("<- {} {} {}/{} {}", msg["verb"], request_id, msg["service"], msg["name"], body);
                                        } else {
                                            println!("<- {} {} {} {}", msg["verb"], request_id, msg["name"], body);
                                        }
                                    } else {
                                        msg["body"] = body.into();
                                    }
                                }
                            }
                        }

                        return Ok(msg);
                    } else {
                        let line = std::str::from_utf8(&buf[s..e])?;
                        let line_regex = Regex::new(r"([^:]+): *(.*)")?;

                        if let None = self.parse_header_line(line, &line_regex, &mut msg) {
                            return Err(format!("MOO: bad header line: {}", line))?;
                        }
                    }
                } else {
                    let line = std::str::from_utf8(&buf[s..e])?;
                    let line_regex = Regex::new(r"^MOO/([0-9]+) ([A-Z]+) (.*)")?;
                    let request_regex = Regex::new(r"([^/]+)/(.*)")?;

                    if let Some(_) = self.parse_first_line(line, &line_regex, &request_regex, &mut msg) {
                        state = "header";
                    } else {
                        return Err(format!("MOO: bad first line: {}", line))?;
                    }
                }

                s = e + 1;
            }

            e += 1;
        }

        Err("MOO: message lacks newline in header")?
    }

    pub fn handle_response(&mut self, msg: &JsonValue) -> Result<(), Box<dyn std::error::Error>> {
        let request_id = msg["request_id"].as_str().unwrap().parse::<u32>()?;

        if let Some(cb) = self.requests.remove(&request_id) {
            let complete = msg["verb"] == "COMPLETE";

            (cb)(self, Some(&msg));

            if !complete {
                self.requests.insert(request_id, cb);
            }
        }

        Ok(())
    }

    pub fn clean_up(&mut self) {
        let mut ids = Vec::new();

        for (id, _) in self.requests.iter() {
            ids.push(*id);
        }

        while let Some(id) = ids.pop() {
            if let Some(cb) = self.requests.remove(&id) {
                (cb)(self, None);
            }
        }
    }

    fn parse_first_line(&self, line: &str, line_regex: &Regex, request_regex: &Regex, msg: &mut JsonValue) -> Option<()> {
        let matches = line_regex.captures(line)?;
        let verb = matches.get(2)?.as_str();

        if verb == "REQUEST" {
            let request = matches.get(3)?.as_str();
            let matches = request_regex.captures(request)?;

            msg["service"] = matches.get(1)?.as_str().into();
            msg["name"] = matches.get(2)?.as_str().into();
        } else {
            msg["name"] = matches.get(3)?.as_str().into();
        }

        msg["verb"] = verb.into();

        Some(())
    }

    fn parse_header_line(&self, line: &str, line_regex: &Regex, msg: &mut JsonValue) -> Option<()> {
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

        Some(())
    }
}

#[derive(Clone)]
pub struct MooMsg {
    pub msg: JsonValue,
    pub mooid: u32,
    pub send_continue: Arc<Mutex<Box<dyn FnMut(&str, Option<&JsonValue>) -> Result<(), Error> + Send>>>,
    pub send_complete: Arc<Mutex<Box<dyn FnMut(&str, Option<&JsonValue>) -> Result<(), Error> + Send>>>
}

impl MooMsg {
    pub fn new(moo: &mut Moo, msg: JsonValue) -> Self {
        let request_id = msg["request_id"].as_str().unwrap().parse::<u32>().unwrap();
        let transport = moo.transport.clone();
        let send_continue = move |name: &str, body: Option<&JsonValue>| {
            let buf = Self::to_raw("CONTINUE", name, request_id, body);

            transport.lock().unwrap().send(&buf[..])
        };

        let transport = moo.transport.clone();
        let send_complete = move |name: &str, body: Option<&JsonValue>| {
            let buf = Self::to_raw("COMPLETE", name, request_id, body);

            transport.lock().unwrap().send(&buf[..])
        };

        Self {
            msg,
            mooid: moo.mooid,
            send_continue: Arc::new(Mutex::new(Box::new(send_continue))),
            send_complete: Arc::new(Mutex::new(Box::new(send_complete)))
        }
    }

    pub fn to_raw(state: &str, name: &str, request_id: u32, body: Option<&JsonValue>) -> Vec<u8> {
        let mut header = format!("MOO/1 {} {}\nRequest-Id: {}\n", state, name, request_id);

        match body {
            Some(body) => {
                let body = json::stringify(body.clone());

                println!("-> {} {} {} {}", state, request_id, name, body);

                let body = body.as_bytes();

                header.push_str(format!("Content-Length: {}\nContent-Type: application/json\n\n", body.len()).as_str());

                [header.as_bytes(), body].concat().to_vec()
            }
            None => {
                println!("-> {} {} {}", state, request_id, name);

                header.push('\n');

                header.as_bytes().to_vec()
            }
        }
    }
}
