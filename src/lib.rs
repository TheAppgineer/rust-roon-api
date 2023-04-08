use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use futures_util::{FutureExt, future::{join_all, select_all, select, Either}};
use moo::{Moo, MooReceiver, MooSender};
use sood::{Message, Sood};
use serde_json::json;
use tokio::select;
use tokio::sync::mpsc::{self, Receiver};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

pub mod moo;
mod sood;

#[cfg(feature = "pairing")]
pub mod pairing;

#[cfg(feature = "status")]
pub mod status;

#[cfg(feature = "transport")]
pub mod transport;

pub const ROON_API_VERSION: &str = env!("CARGO_PKG_VERSION");

type RespProps = (&'static[&'static str], Option<serde_json::Value>);
type Method = Box<dyn Fn(Option<&Core>, Option<&serde_json::Value>) -> RespProps + Send>;

pub struct RoonApi {
    reg_info: serde_json::Value,
    lost_core_id: Arc<Mutex<Option<String>>>,
    #[cfg(feature = "pairing")]
    paired_core: Arc<Mutex<Option<Core>>>
}

#[derive(Clone, Debug)]
pub struct Core {
    pub display_name: String,
    pub display_version: String,
    id: String,
    moo: Moo,
    #[cfg(any(feature = "status", feature = "transport"))]
    services: Option<Vec<Services>>
}

#[derive(Debug)]
pub enum CoreEvent {
    None,
    Found(Core),
    Lost(Core)
}

impl Core {
    #[cfg(feature = "transport")]
    pub fn get_transport(&mut self) -> Option<&mut transport::Transport> {
        if let Some(services) = self.services.as_mut() {
            for svc in services {
                match svc {
                    Services::Transport(transport) => {
                        transport.set_moo(self.moo.clone());

                        return Some(transport)
                    }
                    #[cfg(any(feature = "status"))]
                    _ => ()
                }
            }
        }

        None
    }

    #[cfg(feature = "status")]
    pub fn get_status(&mut self) -> Option<&status::Status> {
        if let Some(services) = self.services.as_mut() {
            for svc in services {
                match svc {
                    Services::Status(status) => {
                        status.set_moo(self.moo.clone());

                        return Some(status)
                    }
                    #[cfg(any(feature = "transport"))]
                    _ => ()
                }
            }
        }

        None
    }
}

impl RoonApi {
    pub fn new(options: serde_json::Value) -> Self {
        let mut reg_info = options;

        reg_info["provided_services"] = json!([]);
        reg_info["required_services"] = json!([]);

        Self {
            reg_info,
            lost_core_id: Arc::new(Mutex::new(None)),
            #[cfg(feature = "pairing")]
            paired_core: Arc::new(Mutex::new(None))
        }
    }

    pub async fn start_discovery(
        &mut self,
        mut provided: HashMap<String, Svc>,
        #[cfg(all(feature = "status", not(any(feature = "transport"))))] services: Option<Vec<Services>>,
        #[cfg(any(feature = "transport"))] mut services: Option<Vec<Services>>,
    ) -> std::io::Result<(Vec<JoinHandle<()>>, Receiver<(CoreEvent, Option<(serde_json::Value, Parsed)>)>)> {
        const SERVICE_ID: &str = "00720724-5143-4a9b-abac-0e50cba674bb";
        const QUERY: [(&str, &str); 1] = [("query_service_id", SERVICE_ID)];
        let mut sood = Sood::new();
        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        let (handle, mut sood_rx) = sood.start().await?;
        let (moo_tx, mut moo_rx) = mpsc::channel::<(Moo, MooSender)>(4);
        let (msg_tx, mut msg_rx) = mpsc::channel::<(usize, Option<serde_json::Value>)>(4);
        let (core_tx, core_rx) = mpsc::channel::<(CoreEvent, Option<(serde_json::Value, Parsed)>)>(4);

        let ping = Ping::new(self);

        provided.insert(ping.name.to_owned(), ping);

        #[cfg(feature = "pairing")]
        {
            let lost_core_id = self.lost_core_id.clone();
            let on_core_lost = move |core_id: String| {
                let mut lost_core_id = lost_core_id.lock().unwrap();
                *lost_core_id = Some(core_id);
            };
            let pairing = pairing::Pairing::new(self, Box::new(on_core_lost));

            provided.insert(pairing.name.to_owned(), pairing);
        }

        #[cfg(any(feature = "transport"))]
        if let Some(services) = &mut services {
            for svc in services {
                match svc {
                    #[cfg(feature = "transport")]
                    Services::Transport(_) => {
                        self.reg_info["required_services"].as_array_mut().unwrap().push(json!(transport::SVCNAME));
                    }
                    #[cfg(any(feature = "status"))]
                    _ => ()
                }
            }
        }

        for (name, _) in &provided {
            self.reg_info["provided_services"].as_array_mut().unwrap().push(json!(name));
        }

        #[cfg(feature = "pairing")]
        let paired_core = self.paired_core.clone();

        let query = async move {
            let mut scan_count = 0;

            loop {
                if scan_count < 6 || scan_count % 6 == 0 {
                    #[cfg(feature = "pairing")]
                    {
                        let paired_core = paired_core.lock().unwrap().to_owned();

                        if let None = paired_core {
                            if let Err(err) = sood.query(&QUERY).await {
                                println!("{}", err);
                            }
                        }
                    }

                    #[cfg(not(feature = "pairing"))]
                    if let Err(err) = sood.query(&QUERY).await {
                        println!("{}", err);
                    }
            }

                scan_count += 1;

                sleep(Duration::from_secs(10)).await;
            }
        };

        let sood_receive = async move {
            fn is_service_response(service_id: &str, msg: &mut Message) -> Option<(String, String)> {
                let svc_id = msg.props.remove("service_id")?;
                let unique_id = msg.props.remove("unique_id")?;
                let port = msg.props.remove("http_port")?;

                if msg.msg_type == 'R' && svc_id == service_id {
                    return Some((unique_id, port));
                }

                None
            }

            let mut moo_receivers: Vec<MooReceiver> = Vec::new();
            let mut sood_conns: Vec<String> = Vec::new();

            loop {
                let mut msg_receivers = Vec::new();
                let mut new_moo = None;
                let mut lost_moo = None;

                if moo_receivers.len() == 0 {
                    if let Some(mut msg) = sood_rx.recv().await {
                        if let Some((unique_id, port)) = is_service_response(SERVICE_ID, &mut msg) {
                            if !sood_conns.contains(&unique_id) {
                                sood_conns.push(unique_id.to_owned());
    
                                if let Ok((moo, mut moo_sender, moo_receiver)) = Moo::new(&msg.ip, &port).await {
                                    moo_sender.send_req("com.roonlabs.registry:1/info", None).await.unwrap();
                                    new_moo = Some(moo_receiver);
                                    moo_tx.send((moo, moo_sender)).await.unwrap();
                                }
                            }
                        }
                    }
                } else {
                    for moo in &mut moo_receivers.iter_mut() {
                        msg_receivers.push(moo.receive_response().boxed());
                    }
    
                    match select(sood_rx.recv().boxed(), select_all(msg_receivers)).await {
                        Either::Left((Some(mut msg), _)) => {
                            if let Some((unique_id, port)) = is_service_response(SERVICE_ID, &mut msg) {
                                if !sood_conns.contains(&unique_id) {
                                    println!("sood connection for unique_id: {}", unique_id);
                                    sood_conns.push(unique_id.to_owned());
        
                                    if let Ok((moo, mut moo_sender, moo_receiver)) = Moo::new(&msg.ip, &port).await {
                                        moo_sender.send_req("com.roonlabs.registry:1/info", None).await.unwrap();
                                        new_moo = Some(moo_receiver);        
                                        moo_tx.send((moo, moo_sender)).await.unwrap();
                                    }
                                }
                            }
                        }
                        Either::Left((None, _)) => (),
                        Either::Right(((Err(_), index, _), _)) => {
                            lost_moo = Some(index);
                            msg_tx.send((index, None)).await.unwrap();
                        }
                        Either::Right(((Ok(msg), index, _), _)) => {
                            msg_tx.send((index, Some(msg))).await.unwrap();
                        }
                    }
                }

                if let Some(moo) = new_moo {
                    moo_receivers.push(moo);
                } else if let Some(index) = lost_moo {
                    moo_receivers.remove(index);

                    sood_conns.remove(index);
                }
            }
        };

        #[cfg(feature = "pairing")]
        let paired_core = self.paired_core.clone();

        let mut body = self.reg_info.clone();
        let lost_core_id = self.lost_core_id.clone();
        let moo_receive = async move {
            let mut moo_senders: Vec<MooSender> = Vec::new();
            let mut cores: Vec<Core> = Vec::new();
            let mut user_moos: HashMap<usize, Moo> = HashMap::new();
            let mut props_option: Option<RespProps> = None;
            let mut response_ids: HashMap<usize, usize> = HashMap::new();
            let mut msg_string: String = String::new();

            loop {
                let mut new_moo = None;
                let mut index_msg: Option<(usize, serde_json::Value)> = None;

                if moo_senders.len() == 0 {
                    new_moo = moo_rx.recv().await;
                } else if response_ids.len() > 0 {
                    let mut msg_senders = Vec::new();
                    let mut index: usize = 0;

                    for moo in moo_senders.iter_mut() {
                        if let Some(request_id) = response_ids.get(&index) {
                            let (hdr, body) = props_option.as_ref().unwrap();

                            if *request_id == 0 {
                                msg_senders.push(moo.send_msg_string(&msg_string).boxed());
                            } else {
                                msg_senders.push(moo.send_msg(*request_id, hdr, body.as_ref()).boxed());
                            }
                        }

                        index += 1;
                    }

                    join_all(msg_senders).await;
                    response_ids.clear();
                } else {
                    let mut req_receivers = Vec::new();

                    for moo in &mut moo_senders.iter_mut() {
                        req_receivers.push(moo.msg_rx.recv().boxed());
                    }

                    select! {
                        Some((index, msg)) = msg_rx.recv() => {
                            match msg {
                                Some(msg) => {
                                    index_msg = Some((index, msg));
                                }
                                None => {
                                    let mut lost_core_id = lost_core_id.lock().unwrap();
                                    *lost_core_id = Some(cores[index].id.to_owned());
                                }
                            }
                        }
                        moo = moo_rx.recv() => {
                            new_moo = moo;
                        }
                        (Some(raw_msg), index, _) = select_all(req_receivers) => {
                            msg_string = raw_msg;
                            response_ids.insert(index, 0);

                            // Restart loop to sent
                            continue;
                        }
                    }
                }

                if let Some((moo, moo_sender)) = new_moo {
                    user_moos.insert(moo_senders.len(), moo);
                    moo_senders.push(moo_sender);
                } else if let Some((index, msg)) = index_msg {
                    if msg["request_id"] == "0" && msg["body"]["core_id"].is_string() {
                        let settings = Self::load_config("roonstate");
                        let moo = moo_senders.get_mut(index).unwrap();

                        if let Some(tokens) = settings.get("tokens") {
                            let core_id = msg["body"]["core_id"].as_str().unwrap();

                            if let Some(token) = tokens.get(core_id) {
                                body["token"] = token.to_owned();
                            }
                        }

                        let req_id = moo.req_id.lock().unwrap().to_owned();

                        props_option = Some((&["REQUEST", "com.roonlabs.registry:1/register"], Some(body.to_owned())));
                        response_ids.insert(index, req_id);
                    } else if msg["name"] == "Registered" {
                        let body = &msg["body"];
                        let moo = user_moos.remove(&index).unwrap();
                        let core = Core {
                            display_name: body["display_name"].as_str().unwrap().to_string(),
                            display_version: body["display_version"].as_str().unwrap().to_string(),
                            id: body["core_id"].as_str().unwrap().to_string(),
                            moo,
                            #[cfg(any(feature = "status", feature = "transport"))]
                            services: services.clone()
                        };
                        let mut settings = Self::load_config("roonstate");

                        settings["tokens"][&core.id] = body["token"].as_str().unwrap().into();

                        #[cfg(feature = "pairing")]
                        {
                            let mut paired_core_id = None;

                            {
                                let mut paired_core = paired_core.lock().unwrap();

                                match paired_core.as_ref() {
                                    None => {
                                        if let Some(set_core_id) = settings.get("paired_core_id") {
                                            if set_core_id.as_str().unwrap() == core.id {
                                                *paired_core = Some(core.to_owned());
                                                paired_core_id = Some(core.id.to_owned());
                                            }
                                        } else {
                                            *paired_core = Some(core.to_owned());
                                            paired_core_id = Some(core.id.to_owned());
                                            settings["paired_core_id"] = core.id.to_owned().into();
                                        }
                                    }
                                    Some(paired_core) => {
                                        if paired_core.id != core.id {
                                            continue;
                                        }
                                    }
                                }
                            }

                            if let Some(paired_core_id) = paired_core_id {
                                let svc_name = pairing::SVCNAME;
                                let svc = provided.remove(svc_name);

                                if let Some(svc) = svc {
                                    {
                                        let sub_types = svc.sub_types.lock().unwrap();
        
                                        for (msg_key, req_id) in sub_types.iter() {
                                            if msg_key.contains("subscribe_pairing") {
                                                let moo_id = msg_key[msg_key.len()..].parse::<usize>().unwrap();
                                                let index = moo_senders.iter().position(|moo| moo.id == moo_id);

                                                if let Some(index) = index {
                                                    response_ids.insert(index, *req_id);
                                                }
                                            }
                                        }
                                    }

                                    provided.insert(svc_name.to_owned(), svc);

                                    let body = json!({"paired_core_id": paired_core_id});
                                    props_option = Some((&["CONTINUE", "Changed"], Some(body)));
                                }
                            }
                        }

                        core_tx.send((CoreEvent::Found(core.clone()), None)).await.unwrap();

                        Self::save_config("roonstate", settings).unwrap();

                        cores.push(core);
                    } else if msg["verb"] == "REQUEST" {
                        let request_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                        let svc_name = msg["service"].as_str().unwrap().to_owned();
                        let svc = provided.remove(&svc_name);

                        if let Some(svc) = svc {
                            let (hdr, body) = (svc.req_handler)(cores.get(index), Some(&msg));

                            if let Some(_) = hdr.get(2) {
                                let sub_name = hdr[0];
                                let sub_types = svc.sub_types.lock().unwrap();

                                for (msg_key, req_id) in sub_types.iter() {
                                    let split: Vec<&str> = msg_key.split(':').collect();

                                    if split[0] == sub_name {
                                        let index = split[1].parse::<usize>().unwrap();
                                        response_ids.insert(index, *req_id);
                                    }
                                }

                                props_option = Some((&hdr[1..], body));
                            } else {
                                props_option = Some((hdr, body));
                                response_ids.insert(index, request_id);
                            }

                            provided.insert(svc_name, svc);
                        } else {
                            let error = format!("{}", msg["name"].as_str().unwrap());
                            let body = json!({"error" : error});

                            props_option = Some((&["COMPLETE", "InvalidRequest"], Some(body)));
                            response_ids.insert(index, request_id);
                        }
                    } else {
                        #[cfg(any(feature = "transport"))]
                        if let Some(svcs) = services.as_ref() {
                            for svc in svcs {
                                match svc {
                                    #[cfg(feature = "transport")]
                                    Services::Transport(transport) => {
                                        let parsed = transport.parse_msg(&msg).await;
                                        core_tx.send((CoreEvent::None, Some((msg, parsed)))).await.unwrap();
                                        break;
                                    }
                                    #[cfg(feature = "status")]
                                    _ => ()
                                }
                            }
                        }
                    }
                } else {
                    let core_id = (*lost_core_id.lock().unwrap()).clone();

                    if let Some(core_id) = core_id {
                        if let Some(index) = cores.iter().position(|core| core.id == core_id) {
                            let core = cores.remove(index);

                            moo_senders.remove(index);
                            core_tx.send((CoreEvent::Lost(core), None)).await.unwrap();
                        }

                        let mut lost_core_id = lost_core_id.lock().unwrap();
                        *lost_core_id = None;
                    }
                }
            }
        };

        handles.push(handle);
        handles.push(tokio::spawn(query));
        handles.push(tokio::spawn(sood_receive));
        handles.push(tokio::spawn(moo_receive));

        Ok((handles, core_rx))
    }

    pub fn register_service(&self, spec: SvcSpec) -> Svc {
        let mut methods = spec.methods;
        let sub_types = Arc::new(Mutex::new(HashMap::new()));
        let mut sub_names = Vec::new();

        for sub in spec.subs {
            let sub_types_clone = sub_types.clone();
            let sub_name = sub.subscribe_name.clone();
            let sub_method = move |core: Option<&Core>, msg: Option<&serde_json::Value>| -> RespProps {
                let mut sub_types = sub_types_clone.lock().unwrap();

                if let Some(msg) = msg {
                    let sub_key = msg["body"]["subscription_key"].as_str().unwrap();
                    let msg_key = format!("{}:{}:{}", sub_name, core.unwrap().moo.id, sub_key);
                    let req_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();

                    sub_types.insert(msg_key, req_id);
                }

                (sub.start)(core, msg)
            };

            let sub_types = sub_types.clone();
            let sub_name = sub.subscribe_name.clone();
            let unsub_method = move |core: Option<&Core>, msg: Option<&serde_json::Value>| -> RespProps {
                let mut sub_types = sub_types.lock().unwrap();

                if let Some(msg) = msg {
                    let sub_key = msg["body"]["subscription_key"].as_str().unwrap();
                    let msg_key = format!("{}:{}:{}", sub_name, core.unwrap().moo.id, sub_key);

                    sub_types.remove(&msg_key);
                }

                if let Some(end) = &sub.end {
                    (end)(core, msg);
                }

                (&["COMPLETE", "Unsubscribed"], None)
            };

            sub_names.push(sub.subscribe_name.clone());
            methods.insert(sub.subscribe_name, Box::new(sub_method));
            methods.insert(sub.unsubscribe_name, Box::new(unsub_method));
        }

        let sub_types_clone = sub_types.clone();
        let req_handler = move |core: Option<&Core>, req: Option<&serde_json::Value>| {
            match req {
                Some(msg) => {
                    let name = msg["name"].as_str().unwrap();

                    if let Some(method) = methods.get(name) {
                        return (method)(core, req);
                    }
                }
                None => {
                    let mut sub_types = sub_types_clone.lock().unwrap();

                    for sub_name in &sub_names {
                        if let Some(core) = core {
                            let sub_mooid = format!("{}:{}", sub_name, core.moo.id);

                            sub_types.retain(|msg_key, _| !msg_key.contains(&sub_mooid));
                        }
                    }
                }
            }

            (&[], None)
        };

        Svc {
            name: spec.name,
            sub_types,
            req_handler: Box::new(req_handler)
        }
    }

    fn save_config(key: &str, value: serde_json::Value) -> std::io::Result<()> {
        let mut config = match Self::read_and_parse("config.json") {
            Some(config) => config,
            None => json!({})
        };

        config[key] = value;

        std::fs::write("config.json", serde_json::to_string_pretty(&config).unwrap())
    }

    fn load_config(key: &str) -> serde_json::Value {
        match Self::read_and_parse("config.json") {
            Some(value) => value.get(key).cloned().into(),
            None => json!({})
        }
    }

    fn read_and_parse(path: &str) -> Option<serde_json::Value> {
        let content = std::fs::read(path).ok()?;
        let content = std::str::from_utf8(&content).ok()?;

        serde_json::from_str::<serde_json::Value>(content).ok()
    }
}

pub struct Ping;

impl Ping {
    pub fn new(roon: &RoonApi) -> Svc {
        const SVCNAME: &str = "com.roonlabs.ping:1";
        let mut spec = SvcSpec::new(SVCNAME);
        let ping = |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            (&["COMPLETE", "Success"], None)
        };
    
        spec.add_method("ping", Box::new(ping));

        roon.register_service(spec)
    }
}

pub struct Sub {
    subscribe_name: String,
    unsubscribe_name: String,
    start: Method,
    end: Option<Method>
}

pub struct SvcSpec {
    name: &'static str,
    methods: HashMap<String, Method>,
    subs: Vec<Sub>
}

impl SvcSpec {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            methods: HashMap::new(),
            subs: Vec::new()
        }
    }

    pub fn add_method(&mut self, name: &str, method: Method) {
        self.methods.insert(name.to_owned(), method);
    }

    pub fn add_sub(&mut self, sub: Sub) {
        self.subs.push(sub);
    }
}

pub struct Svc {
    pub sub_types: Arc<Mutex<HashMap<String, usize>>>,
    name: &'static str,
    req_handler: Method
}

#[derive(Clone, Debug)]
pub enum Services {
    #[cfg(feature = "transport")]
    Transport(transport::Transport),
    #[cfg(feature = "status")]
    Status(status::Status),
}

#[cfg(any(feature = "transport"))]
#[derive(Debug)]
pub enum Parsed {
    None,
    Zones(Vec<transport::Zone>),
    ZonesSeek(Vec<transport::ZoneSeek>),
    ZonesRemoved(Vec<String>),
    Outputs(Vec<transport::Output>),
    OutputsRemoved(Vec<String>),
    Queue(Vec<transport::QueueItem>)
}

#[cfg(not(any(feature = "transport")))]
#[derive(Debug)]
pub enum Parsed {
    None
}

#[cfg(test)]
#[cfg(not(any(feature = "pairing", feature = "status")))]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        let info = json!({
            "extension_id": "com.theappgineer.rust-roon-api",
            "display_name": "Rust Roon API",
            "display_version": ROON_API_VERSION,
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let mut roon = RoonApi::new(info);
        let provided: HashMap<String, Svc> = HashMap::new();
        let (mut handles, mut core_rx) = roon.start_discovery(provided).await.unwrap();

        handles.push(tokio::spawn(async move {
            loop {
                if let Some((core, _)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(core) => {
                            println!("Core found: {}, version {}", core.display_name, core.display_version);
                        }
                        CoreEvent::Lost(core) => {
                            println!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }
                }
            }
        }));

        for handle in handles {
            handle.await.unwrap();
        }
    }
}
