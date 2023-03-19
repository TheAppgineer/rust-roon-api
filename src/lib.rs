use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use futures_util::{FutureExt, future::{join_all, select_all, select, Either}};
use moo::{Moo, MooReceiver, MooSender};
use sood::{Message, Sood};
use serde_json::json;
use tokio::select;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

pub mod moo;
mod sood;

#[cfg(feature = "status")]
pub mod status;

#[cfg(feature = "transport")]
pub mod transport;

type RespProps = (&'static[&'static str], Option<serde_json::Value>);
type CoreEvent = dyn Fn(&Core) + Send;
type Method = Box<dyn Fn(Option<&Core>, Option<&serde_json::Value>) -> RespProps + Send>;

pub struct RoonApi {
    reg_info: serde_json::Value,
    on_core_found: Arc<Mutex<CoreEvent>>,
    on_core_lost: Arc<Mutex<CoreEvent>>,
    #[cfg(feature = "pairing")]
    paired_core: Arc<Mutex<Option<Core>>>
}

#[derive(Clone, Debug)]
pub struct Core {
    pub display_name: String,
    pub display_version: String,
    core_id: String,
    moo: Moo
}

impl RoonApi {
    pub fn new(options: serde_json::Value, on_core_found: Box<CoreEvent>, on_core_lost: Box<CoreEvent>) -> Self {
        let mut reg_info = options;

        reg_info["provided_services"] = json!([]);
        reg_info["required_services"] = json!([]);

        Self {
            reg_info,
            on_core_found: Arc::new(Mutex::new(on_core_found)),
            on_core_lost: Arc::new(Mutex::new(on_core_lost)),
            #[cfg(feature = "pairing")]
            paired_core: Arc::new(Mutex::new(None))
        }
    }

    pub async fn start_discovery(
        &mut self,
        mut provided: HashMap<String, Svc>,
        #[cfg(feature = "pairing")] mut required: Vec<Box<dyn RequiredSvc + 'static>>,
    ) -> std::io::Result<Vec<JoinHandle<()>>> {
        const SERVICE_ID: &str = "00720724-5143-4a9b-abac-0e50cba674bb";
        const QUERY: [(&str, &str); 1] = [("query_service_id", SERVICE_ID)];
        let mut sood = Sood::new();
        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        let (handle, mut sood_rx) = sood.start().await?;
        let (moo_tx, mut moo_rx) = mpsc::channel::<(Moo, MooSender)>(4);
        let (msg_tx, mut msg_rx) = mpsc::channel::<(usize, Option<serde_json::Value>)>(4);

        for (name, _) in &provided {
            self.reg_info["provided_services"].as_array_mut().unwrap().push(json!(name));
        }

        #[cfg(feature = "pairing")]
        for svc in &required {
            self.reg_info["required_services"].as_array_mut().unwrap().push(json!(svc.get_name()));
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
                        Either::Right(((Err(_), moo_id, _), _)) => {
                            lost_moo = Some(moo_id);
                            msg_tx.send((moo_id, None)).await.unwrap();
                        }
                        Either::Right(((Ok(msg), moo_id, _), _)) => {
                            msg_tx.send((moo_id, Some(msg))).await.unwrap();
                        }
                    }
                }

                if let Some(moo) = new_moo {
                    moo_receivers.push(moo);
                } else if let Some(moo_id) = lost_moo {
                    moo_receivers.remove(moo_id);

                    sood_conns.remove(moo_id);
                }
            }
        };

        #[cfg(feature = "pairing")]
        let paired_core = self.paired_core.clone();

        let mut body = self.reg_info.clone();
        let on_core_found = self.on_core_found.clone();
        let on_core_lost = self.on_core_lost.clone();
        let moo_receive = async move {
            let mut moo_senders: Vec<MooSender> = Vec::new();
            let mut cores: Vec<Core> = Vec::new();
            let mut user_moos: HashMap<usize, Moo> = HashMap::new();
            let mut props_option: Option<RespProps> = None;
            let mut response_ids: HashMap<usize, usize> = HashMap::new();
            let mut req_name: String = String::new();

            loop {
                let mut new_moo = None;
                let mut lost_moo = None;
                let mut mooid_msg: Option<(usize, serde_json::Value)> = None;

                if moo_senders.len() == 0 {
                    new_moo = moo_rx.recv().await;
                } else {
                    if response_ids.len() > 0 {
                        let mut msg_senders = Vec::new();

                        for moo in &mut moo_senders.iter_mut() {
                            if let Some(request_id) = response_ids.get(&moo.id) {
                                let (hdr, body) = props_option.as_ref().unwrap();

                                if *request_id == 0 {
                                    msg_senders.push(moo.send_req(&req_name, body.as_ref()).boxed());
                                } else {
                                    msg_senders.push(moo.send_msg(*request_id, hdr, body.as_ref()).boxed());
                                }
                            }
                        }

                        for moo_id in join_all(msg_senders).await {
                            if let Ok(moo_id) = moo_id {
                                response_ids.remove(&moo_id);
                            }
                        }
                    } else {
                        let mut req_receivers = Vec::new();

                        for moo in &mut moo_senders.iter_mut() {
                            req_receivers.push(moo.msg_rx.recv().boxed());
                        }

                        select! {
                            Some((moo_id, msg)) = msg_rx.recv() => {
                                match msg {
                                    Some(msg) => {
                                        mooid_msg = Some((moo_id, msg));
                                    }
                                    None => {
                                        lost_moo = Some(moo_id);
                                    }
                                }
                            }
                            moo = moo_rx.recv() => {
                                new_moo = moo;
                            }
                            (Some((name, body)), index, _) = select_all(req_receivers) => {
                                req_name = name.to_owned();
                                props_option = Some((&["REQUEST"], body));
                                response_ids.insert(index, 0);
                            }
                        }
                    }
                }

                if let Some((moo, moo_sender)) = new_moo {
                    moo_senders.push(moo_sender);
                    user_moos.insert(moo.id, moo);
                } else if let Some(moo_id) = lost_moo {
                    let core = cores.remove(moo_id);
                    let on_core_lost = on_core_lost.lock().unwrap();

                    moo_senders.remove(moo_id);
                    on_core_lost(&core);
                } else if let Some((index, msg)) = mooid_msg {
                    if msg["request_id"] == "0" && msg["body"]["core_id"].is_string() {
                        let settings = Self::load_config("roonstate");
                        let moo = moo_senders.get_mut(index).unwrap();

                        if let Some(tokens) = settings.get("tokens") {
                            let core_id = msg["body"]["core_id"].as_str().unwrap();

                            if let Some(token) = tokens.get(core_id) {
                                body["token"] = token.to_owned();
                            }
                        }

                        props_option = Some((&["REQUEST", "com.roonlabs.registry:1/register"], Some(body.to_owned())));
                        response_ids.insert(index, moo.req_id);
                    } else if msg["name"] == "Registered" {
                        let body = &msg["body"];
                        let moo = user_moos.remove(&index).unwrap();
                        let core = Core {
                            display_name: body["display_name"].as_str().unwrap().to_string(),
                            display_version: body["display_version"].as_str().unwrap().to_string(),
                            core_id: body["core_id"].as_str().unwrap().to_string(),
                            moo
                        };
                        let mut settings = Self::load_config("roonstate");

                        settings["tokens"][&core.core_id] = body["token"].as_str().unwrap().into();

                        #[cfg(feature = "pairing")]
                        {
                            let mut paired_core_id = None;

                            {
                                let mut paired_core = paired_core.lock().unwrap();

                                match &*paired_core {
                                    None => {
                                        if let Some(set_core_id) = settings.get("paired_core_id") {
                                            if set_core_id.as_str().unwrap() == core.core_id {
                                                *paired_core = Some(core.to_owned());
                                                paired_core_id = Some(core.core_id.to_owned());
                                            }
                                        } else {
                                            *paired_core = Some(core.to_owned());
                                            paired_core_id = Some(core.core_id.to_owned());
                                            settings["paired_core_id"] = core.core_id.to_owned().into();
                                        }
                                    }
                                    Some(paired_core) => {
                                        if paired_core.core_id == core.core_id {
                                            let on_core_found = on_core_found.lock().unwrap();
                                            on_core_found(&core);
                                        }
                                    }
                                }
                            }

                            if let Some(paired_core_id) = paired_core_id {
                                let svc_name = "com.roonlabs.pairing:1";
                                let svc = provided.remove(svc_name);

                                if let Some(svc) = svc {
                                    {
                                        let sub_types = svc.sub_types.lock().unwrap();
        
                                        for (msg_key, msg) in sub_types.iter() {
                                            if msg_key.contains("subscribe_pairing") {
                                                let request_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                                                let moo_id = msg_key[msg_key.len()..].parse::<usize>().unwrap();
                                                response_ids.insert(moo_id, request_id);
                                            }
                                        }
                                    }

                                    provided.insert(svc_name.to_owned(), svc);

                                    let body = json!({"paired_core_id": paired_core_id});
                                    props_option = Some((&["CONTINUE", "Changed"], Some(body)));
                                }

                                for svc in &mut required {
                                    svc.set_core(paired_core.clone());
                                }

                                let on_core_found = on_core_found.lock().unwrap();
                                on_core_found(&core);
                            }
                        }

                        #[cfg(not(feature = "pairing"))]
                        {
                            let on_core_found = on_core_found.lock().unwrap();
                            on_core_found(&core);
                        }

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

                                for (msg_key, msg) in sub_types.iter() {
                                    if msg_key.contains(sub_name) {
                                        let request_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                                        let moo_id = msg_key[msg_key.len()..].parse::<usize>().unwrap();
                                        response_ids.insert(moo_id, request_id);
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
                    }
                }
            }
        };

        handles.push(handle);
        handles.push(tokio::spawn(query));
        handles.push(tokio::spawn(sood_receive));
        handles.push(tokio::spawn(moo_receive));

        Ok(handles)
    }

    pub fn init_services(&mut self, provided: &mut HashMap<String, Svc>) {
        self.add_ping_service(provided);

        #[cfg(feature = "pairing")]
        self.add_pairing_service(provided);
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

                    sub_types.insert(msg_key, msg.to_owned());
                }

                (sub.start)(core, msg)
            };

            let sub_types = sub_types.clone();
            let sub_name = sub.subscribe_name.clone();
            let unsub_method = move |core: Option<&Core>, msg: Option<&serde_json::Value>| -> RespProps {
                let mut sub_types = sub_types.lock().unwrap();
                let sub_mooid = format!("{}{}", sub_name, core.unwrap().moo.id);

                sub_types.remove(&sub_mooid);

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
                            let sub_mooid = format!("{}{}", sub_name, core.moo.id);
                            sub_types.remove(&sub_mooid);
                        }
                    }
                }
            }

            (&[], None)
        };

        Svc {
            sub_types,
            req_handler: Box::new(req_handler)
        }
    }

    fn add_ping_service(&mut self, svcs: &mut HashMap<String, Svc>) {
        let mut spec = SvcSpec::new();
        let ping = |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            (&["COMPLETE", "Success"], None)
        };

        spec.add_method("ping", Box::new(ping));
        svcs.insert("com.roonlabs.ping:1".to_owned(), self.register_service(spec));
    }
 
    #[cfg(feature = "pairing")]
    fn add_pairing_service(&mut self, svcs: &mut HashMap<String, Svc>) {
        let mut spec = SvcSpec::new();
        let paired_core = self.paired_core.clone();
        let get_pairing = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            match &*paired_core.lock().unwrap() {
                Some(paired_core) => {
                    let body = json!({"paired_core_id": paired_core.core_id});

                    (&["COMPLETE", "Success"], Some(body))
                }
                None => {
                    (&["COMPLETE", "Success"], None)
                }
            }
        };

        spec.add_method("get_pairing", Box::new(get_pairing));

        let paired_core = self.paired_core.clone();
        let on_core_found = self.on_core_found.clone();
        let on_core_lost = self.on_core_lost.clone();
        let pair = move |core: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            if let Some(core) = core {
                let mut paired_core = paired_core.lock().unwrap();

                if let Some(paired_core) = paired_core.as_ref() {
                    if paired_core.core_id == core.core_id {
                        return (&[], None)
                    } else {
                        let on_core_lost = on_core_lost.lock().unwrap();
    
                        on_core_lost(paired_core);
                    }
                }
    
                let mut settings = Self::load_config("roonstate");
    
                settings["paired_core_id"] = core.core_id.clone().into();
                Self::save_config("roonstate", settings).unwrap();
    
                *paired_core = Some(core.to_owned());
    
                let on_core_found = on_core_found.lock().unwrap();
                on_core_found(&core);
    
                let body = json!({"paired_core_id": core.core_id});
    
                (&["subscribe_pairing", "CONTINUE", "Changed"], Some(body))
            } else {
                (&[], None)
            }
        };

        spec.add_method("pair", Box::new(pair));

        let paired_core = self.paired_core.clone();
        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            match &*paired_core.lock().unwrap() {
                Some(paired_core) => {
                    let body = json!({"paired_core_id": paired_core.core_id});

                    (&["CONTINUE", "Subscribed"], Some(body))
                }
                None => {
                    let body = json!({"paired_core_id": "undefined"});

                    (&["CONTINUE", "Subscribed"], Some(body))
                }
            }
        };

        spec.add_sub(Sub {
            subscribe_name: "subscribe_pairing".to_owned(),
            unsubscribe_name: "unsubscribe_pairing".to_owned(),
            start: Box::new(start),
            end: None
        });
        svcs.insert("com.roonlabs.pairing:1".to_owned(), self.register_service(spec));
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

pub struct Sub {
    subscribe_name: String,
    unsubscribe_name: String,
    start: Method,
    end: Option<Method>
}

pub enum SvcType {
    Provides,
    Requires
}

pub struct SvcSpec {
    methods: HashMap<String, Method>,
    subs: Vec<Sub>
}

impl SvcSpec {
    pub fn new() -> Self {
        Self {
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
    sub_types: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    req_handler: Method
}

pub trait RequiredSvc: Send {
    fn get_name(&self) -> &str;
    fn set_core(&mut self, core: Arc<Mutex<Option<Core>>>);
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
            "display_version": "0.1.0",
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let on_core_found = move |core: &Core| {
            println!("Core found: {}, version {}", core.display_name, core.display_version);
        };
        let on_core_lost = move |core: &Core| {
            println!("Core lost: {}", core.display_name);
        };
        let mut roon = RoonApi::new(info, Box::new(on_core_found), Box::new(on_core_lost));
        let mut provided: HashMap<String, Svc> = HashMap::new();

        roon.init_services(&mut provided);

        for handle in roon.start_discovery(provided).await.unwrap() {
            handle.await.unwrap();
        }
    }
}
