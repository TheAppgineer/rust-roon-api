use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::io::ErrorKind;
use std::collections::HashMap;
use std::str::from_utf8;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc::{self, Receiver}, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

const SOOD_PORT: u16 = 9003;
const SOOD_MULTICAST_IP: [u8; 4] = [239, 255, 90, 90];

pub struct Sood {
    multicast: Arc<Mutex<HashMap<Ipv4Addr, Multicast>>>,
    unicast: Option<Arc<Mutex<Unicast>>>,
    iface_seq: i32
}

pub struct Message {
    pub ip: IpAddr,
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
    pub fn new() -> Self {
        Self {
            multicast: Arc::new(Mutex::new(HashMap::new())),
            unicast: None,
            iface_seq: 0
        }
    }

    pub async fn start(&mut self) -> std::io::Result<(JoinHandle<()>, Receiver<Message>)> {
        self.init_socket().await?;

        let unicast = self.unicast.clone();
        let multicast = self.multicast.clone();
        let (tx, rx) = mpsc::channel::<Message>(4);

        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 1024];

            loop {
                if let Some(unicast) = &unicast {
                    match unicast.lock().await.send_sock.try_recv_from(&mut buf) {
                        Ok((size, from)) => {
                            let buf = &buf[..size];
    
                            if let Some(msg)= Message::new(buf, from) {
                                if let Err(err) = tx.send(msg).await {
                                    println!("{}", err);
                                }
                            }
                        }
                        Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                        }
                        Err(err) => {
                            println!("{}", err);
                        }
                    }
                }
    
                for (_, mc) in &*multicast.lock().await {
                    match mc.send_sock.try_recv_from(&mut buf) {
                        Ok((size, from)) => {
                            let buf = &buf[..size];
    
                            if let Some(msg)= Message::new(buf, from) {
                                if let Err(err) = tx.send(msg).await{
                                    println!("{}", err);
                                }
                            }
                        }
                        Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                        }
                        Err(err) => {
                            println!("{}", err);
                        }
                    }
    
                    match mc.recv_sock.try_recv_from(&mut buf) {
                        Ok((size, from)) => {
                            let buf = &buf[..size];
    
                            if let Some(msg)= Message::new(buf, from) {
                                if let Err(err) = tx.send(msg).await{
                                    println!("{}", err);
                                }
                            }
                        }
                        Err(ref err) if err.kind() == ErrorKind::WouldBlock => {
                        }
                        Err(err) => {
                            println!("{}", err);
                        }
                    }
                }

                sleep(Duration::from_millis(10)).await;
            }
        });

        Ok((handle, rx))
    }

    pub async fn query(&self, props: &[(&str, &str)]) -> std::io::Result<()> {
        const TID: &str = "_tid";
        let mut vec = Vec::new();
        let mut has_tid = false;

        for (key, value) in props {
            vec.push(key.len() as u8);
            vec = [vec, Vec::from(*key)].concat();
            vec.push((value.len() >> 8) as u8);
            vec.push((value.len() & 0xFF) as u8);
            vec = [vec, Vec::from(*value)].concat();
    
            if *key == TID {
                has_tid = true;
            }
        }

        if !has_tid {
            let uuid = Uuid::new_v4().to_string();
            let mut tid: Vec<u8> = Vec::new();

            tid.push(TID.len() as u8);
            tid = [tid, Vec::from(TID)].concat();
            tid.push((uuid.len() >> 8) as u8);
            tid.push((uuid.len() & 0xFF) as u8);
            vec = [tid, Vec::from(uuid), vec].concat();
        }

        vec = [Vec::from("SOOD\u{2}Q"), vec].concat();

        let ip_addr = IpAddr::V4(Ipv4Addr::from(SOOD_MULTICAST_IP));
        let addr = SocketAddr::new(ip_addr, SOOD_PORT);
        let multicast = self.multicast.lock().await;

        for (_, mc) in &*multicast {
            if let Err(err) = mc.send_sock.try_send_to(&vec, addr) {
                if err.kind() != ErrorKind::WouldBlock {
                    return Err(err);
                }
            }

            let ip_addr = IpAddr::V4(mc.broadcast);
            let addr = SocketAddr::new(ip_addr, SOOD_PORT);

            if let Err(err) = mc.send_sock.try_send_to(&vec, addr) {
                if err.kind() != ErrorKind::WouldBlock {
                    return Err(err);
                }
            }
        }

        if let Some(unicast) = &self.unicast {
            let unicast = unicast.lock().await;

            if let Err(err) = unicast.send_sock.try_send_to(&vec, addr) {
                if err.kind() != ErrorKind::WouldBlock {
                    return Err(err);
                }
            }
        }

        Ok(())
    }

    async fn init_socket(&mut self) -> std::io::Result<bool> {
        let mut iface_change = false;

        self.iface_seq += 1;

        if let Ok(iface) = if_addrs::get_if_addrs() {
            for iface in iface {
                if !iface.is_loopback() {
                    if let if_addrs::IfAddr::V4(address) = &iface.addr {
                        iface_change |= self.listen_iface(address.ip, address.netmask).await;
                    }
                }
            }
        }

        let mut multicast = self.multicast.lock().await;
        let initial_len = multicast.len();

        multicast.retain(|_, mcast| mcast.seq == self.iface_seq);
        iface_change |= multicast.len() != initial_len;

        if let None = self.unicast {
            self.unicast = Some(Arc::new(Mutex::new(Unicast::new().await?)));
        }

        sleep(Duration::from_millis(200)).await;

        Ok(iface_change)
    }

    async fn listen_iface(&mut self, ip: Ipv4Addr, netmask: Ipv4Addr) -> bool {
        let mut new_iface = false;
        let mut multicast = self.multicast.lock().await;

        if multicast.contains_key(&ip) {
            multicast.get_mut(&ip).unwrap().seq = self.iface_seq;
        } else {
            if let Ok(mc) = Multicast::new(self.iface_seq, ip, netmask).await {
                multicast.insert(ip, mc);

                new_iface = true;
            }
        }

        new_iface
    }
}

impl Message {
    fn new(buf: &[u8], from: SocketAddr) -> Option<Self> {
        if from_utf8(&buf[0..5]).unwrap() == "SOOD\u{2}" {
            let msg_type = from_utf8(&buf[5..6]).unwrap().chars().next().unwrap();
            let mut pos = 6;
            let mut props: HashMap<String, String> = HashMap::new();

            while pos < buf.len() {
                let mut len: usize = buf[pos] as usize;
                pos += 1;

                if len == 0 || pos + len > buf.len() {
                    return None;
                }

                let name = from_utf8(&buf[pos..pos+len]).unwrap();

                pos += len;
                len = ((buf[pos] as usize) << 8) | (buf[pos + 1] as usize);

                if pos + len > buf.len() {
                    return None;
                }
                pos += 2;

                let value = if len == 0 {
                    ""
                } else {
                    from_utf8(&buf[pos..pos+len]).unwrap()
                };

                pos += len;
                props.insert(name.to_string(), value.to_string());
            }

            Some(Message {
                ip: from.ip(),
                msg_type,
                props
            })
        } else {
            None
        }
    }
}

impl Multicast {
    async fn new(seq: i32, ip: Ipv4Addr, netmask: Ipv4Addr) -> std::io::Result<Self> {
        let ip_octets = ip.octets();
        let netmask_octets = netmask.octets();
        let mut broadcast_octets = [0u8; 4];

        for index in 0..4 {
            broadcast_octets[index] = ip_octets[index] | (netmask_octets[index] ^ 255);
        }

        let recv_sock = UdpSocket::bind(SocketAddr::from(([0; 4], SOOD_PORT))).await?;
        let send_sock = UdpSocket::bind(SocketAddr::from((ip_octets, 0))).await?;
        let broadcast = Ipv4Addr::from(broadcast_octets);

        recv_sock.join_multicast_v4(Ipv4Addr::from(SOOD_MULTICAST_IP), ip)?;
        send_sock.set_broadcast(true)?;
        send_sock.set_multicast_ttl_v4(1)?;

        Ok(Self {
            recv_sock,
            send_sock,
            broadcast,
            seq
        })
    }
}

impl Unicast {
    async fn new() -> std::io::Result<Self> {
        let send_sock = UdpSocket::bind("0.0.0.0:0").await?;

        send_sock.set_broadcast(true)?;
        send_sock.set_multicast_ttl_v4(1)?;

        Ok(Self {
            send_sock
        })
    }
}
