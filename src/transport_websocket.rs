use std::net::IpAddr;
use url::Url;
use tungstenite::{Error, WebSocket, Message, stream};

pub struct Transport {
    ws: WebSocket<stream::MaybeTlsStream<std::net::TcpStream>>
}

impl Transport {
    pub fn new(ip: IpAddr, port: &String) -> Result<Self, Error> { //Result<(), Box<dyn Error>> {
        let url = format!("ws://{}:{}/api", ip, port);

        match tungstenite::connect(Url::parse(&url).unwrap()) {
            Ok((mut ws, _response)) =>  {
                if let stream::MaybeTlsStream::Plain(s) = ws.get_mut() {
                    s.set_nonblocking(true).unwrap();
                }
                Ok(Self {
                    ws
                })
            }
            Err(error) => {
                Err(error)
            }
        }
    }

    pub fn send(&mut self, buf: &[u8]) -> Result<(), tungstenite::Error> {
        self.ws.write_message(Message::Binary(Vec::from(buf)))
    }

    pub fn receive(&mut self) -> Result<Vec<u8>, tungstenite::Error> {
        match self.ws.read_message() {
            Ok(message) => {
                match message {
                    Message::Binary(buf) => {
                        Ok(buf)
                    }
                    _ => {
                        panic!("{:?}", message)
                    }
                }
            }
            Err(error) => {
                Err(error)
            }
        }
    }
}
