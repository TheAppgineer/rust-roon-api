use std::net::{IpAddr, Ipv4Addr, UdpSocket, SocketAddr};
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};
use std::sync::{Arc, Mutex, mpsc};
use uuid::Uuid;

const SOOD_PORT: u16 = 9003;
const SOOD_MULTICAST_IP: [u8; 4] = [239, 255, 90, 90];
const LISTEN_ALL_RANDOM_PORT: &str = "0.0.0.0:0";

pub struct Sood {
    multicast: Arc<Mutex<HashMap<Ipv4Addr, Multicast>>>,
    unicast: Arc<Mutex<Unicast>>,
    iface_seq: i32
}

pub struct Message {
    pub from: SocketAddr,
    pub msg_type: char,
    pub props: HashMap<String, String>
}

struct Multicast {
    recv_sock: UdpSocket,
    send_sock: UdpSocket,
    broadcast: Ipv4Addr,
    seq: i32
}

struct Unicast {
    send_sock: UdpSocket
}

impl Sood {
    pub fn new () -> std::io::Result<Self> {
        Ok(Self {
            multicast: Arc::new(Mutex::new(HashMap::new())),
            unicast: Arc::new(Mutex::new(Unicast::new()?)),
            iface_seq: 0
        })
    }

    pub fn start<F: FnMut(&Sood)>(&mut self, mut on_startup: F) -> (JoinHandle<()>, Receiver<Message>) {
        let iface_change = self.init_socket();
        let unicast = self.unicast.clone();
        let multicast = self.multicast.clone();
        let (tx, rx) = mpsc::channel::<Message>();

        let handle = thread::spawn(move || loop {
            if let Some(msg) = Sood::recv_msg(&unicast.lock().unwrap().send_sock) {
                if let Err(error) = tx.send(msg) {
                    println!("{}", error);
                }
            };
            
            for (_, mc) in &*multicast.lock().unwrap() {
                if let Some(msg) = Sood::recv_msg(&mc.send_sock) {
                    println!("Multicast send_sock receive !!!");
                    if let Err(error) = tx.send(msg) {
                        println!("{}", error);
                    }
                };

                if let Some(msg) = Sood::recv_msg(&mc.recv_sock) {
                    if let Err(error) = tx.send(msg) {
                        println!("{}", error);
                    }
                };
            }
    
            std::thread::sleep(core::time::Duration::from_millis(10));
        });

        (on_startup)(self);

        if iface_change {
            // TODO emit network
        }

        (handle, rx)
    }

    pub fn query(&self, props: HashMap<&str, &str>) {
        let mut buf = [0u8; 1024];
        let mut pos = 0;

        for byte in "SOOD\u{2}Q".bytes() {
            buf[pos] = byte;
            pos += 1;
        }

        if !props.contains_key("_tid") {
            (buf, pos) = Sood::add_to_buffer(buf, pos, "_tid", Uuid::new_v4().to_string().as_str());
        }

        for (name, value) in props.iter() {
            (buf, pos) = Sood::add_to_buffer(buf, pos, *name, *value);
        }

        let multicast = self.multicast.lock().unwrap();
        let ip_addr = IpAddr::V4(Ipv4Addr::from(SOOD_MULTICAST_IP));
        let addr = SocketAddr::new(ip_addr, SOOD_PORT);

        for (_, mc) in &*multicast {
            mc.send_sock.send_to(&buf[0..pos], addr).unwrap();

            let ip_addr = IpAddr::V4(mc.broadcast);
            let addr = SocketAddr::new(ip_addr, SOOD_PORT);

            if let Err(error) = mc.send_sock.send_to(&buf[0..pos], addr) {
                println!("{} {}", addr, error);
            }
        }

        let unicast = self.unicast.lock().unwrap();
        unicast.send_sock.send_to(&buf[0..pos], addr).unwrap();
    }

    fn init_socket(&mut self) -> bool {
        self.iface_seq += 1;
        let mut iface_change = false;
        let list = get_if_addrs::get_if_addrs().unwrap();

        for iface in list {
            if !iface.is_loopback() {
                if let get_if_addrs::IfAddr::V4(ifv4addr) = iface.addr {
                    iface_change |= self.listen_iface(ifv4addr.ip, ifv4addr.netmask);
                }
            }
        }

        let mut multicast = self.multicast.lock().unwrap();
        let initial_len = multicast.len();

        multicast.retain(|_, mcast| mcast.seq == self.iface_seq);
        iface_change |= multicast.len() != initial_len;

        iface_change
    }

    fn listen_iface(&mut self, ip: Ipv4Addr, netmask: Ipv4Addr) -> bool {
        let mut new_iface = false;

        let mut multicast = self.multicast.lock().unwrap();
        if multicast.contains_key(&ip) {
            multicast.get_mut(&ip).unwrap().seq = self.iface_seq;
        } else {
            if let Ok(mc) = Multicast::new(self.iface_seq, ip, netmask) {
                multicast.insert(ip, mc);
            }

            new_iface = true;
        }

        new_iface
    }

    fn add_to_buffer(mut buf: [u8; 1024], mut pos: usize, name: &str, value: &str) -> ([u8; 1024], usize) {
        buf[pos] = name.len() as u8;
        pos += 1;

        for byte in name.bytes() {
            buf[pos] = byte;
            pos += 1;
        }

        buf[pos] = (value.len() >> 8) as u8;
        pos += 1;
        buf[pos] = (value.len() & 0xFF) as u8;
        pos += 1;

        for byte in value.bytes() {
            buf[pos] = byte;
            pos += 1;
        }

        (buf, pos)
    }

    fn recv_msg(socket: &UdpSocket) -> Option<Message> {
        let mut buf = [0u8; 1024];

        match socket.recv_from(&mut buf) {
            Err(error) => {
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    panic!("{}", error);
                } else {
                    None
                }
            }
            Ok((usize, from)) => {
                let buf = &buf[..usize];

                if std::str::from_utf8(&buf[0..5]).unwrap() == "SOOD\u{2}" {
                    let msg_type = std::str::from_utf8(&buf[5..6]).unwrap().chars().next().unwrap();
                    let mut pos = 6;
                    let mut props: HashMap<String, String> = HashMap::new();
        
                    while pos < buf.len() {
                        let mut len: usize = buf[pos] as usize;
                        pos += 1;
        
                        if len == 0 || pos + len > buf.len() {
                            return None;
                        }
        
                        let name = std::str::from_utf8(&buf[pos..pos+len]).unwrap();
        
                        pos += len;
                        len = ((buf[pos] as usize) << 8) | (buf[pos + 1] as usize);
        
                        if pos + len > buf.len() {
                            return None;
                        }
                        pos += 2;
        
                        let value = if len == 0 {
                            ""
                        } else {
                            std::str::from_utf8(&buf[pos..pos+len]).unwrap()
                        };
        
                        pos += len;
                        props.insert(name.to_string(), value.to_string());
                    }
        
                    Some(Message {
                        from,
                        msg_type,
                        props
                    })
                } else {
                    None
                }
            }
        }
    }
}

impl Multicast {
    fn new(seq: i32, ip: Ipv4Addr, netmask: Ipv4Addr) -> std::io::Result<Self> {
        let ip_octets = ip.octets();
        let netmask_octets = netmask.octets();
        let mut broadcast_octets = [0u8; 4];

        for index in 0..4 {
            broadcast_octets[index] = ip_octets[index] | (netmask_octets[index] ^ 255);
        }

        let recv_sock = UdpSocket::bind(std::net::SocketAddr::from(([0; 4], SOOD_PORT)))?;
        let send_sock = UdpSocket::bind(std::net::SocketAddr::from((ip.octets(), 0)))?;
        let broadcast = Ipv4Addr::from(broadcast_octets);

        send_sock.set_nonblocking(true)?;
        send_sock.set_broadcast(true)?;
        send_sock.set_multicast_ttl_v4(1)?;

        recv_sock.set_nonblocking(true)?;
        recv_sock.join_multicast_v4(&Ipv4Addr::from(SOOD_MULTICAST_IP), &ip)?;

        Ok(Self {
            recv_sock,
            send_sock,
            broadcast,
            seq
        })
    }    
}

impl Unicast {
    fn new() -> std::io::Result<Self> {
        let send_sock = UdpSocket::bind(LISTEN_ALL_RANDOM_PORT)?;

        send_sock.set_broadcast(true)?;
        send_sock.set_multicast_ttl_v4(1)?;
        send_sock.set_nonblocking(true)?;

        Ok(Self {
            send_sock
        })
    }
}
