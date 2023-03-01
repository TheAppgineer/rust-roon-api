use std::collections::HashMap;
use std::sync::Arc;
use futures_util::{FutureExt, future::{select_all, select, Either}};
use moo::{Moo, RespProps};
use sood::{Message, Sood};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

mod moo;
mod sood;

type StdMutex<T> = std::sync::Mutex<T>;
type TokioMutex<T> = tokio::sync::Mutex<T>;
type Method = Box<dyn Fn(&Core, Option<&serde_json::Value>) -> RespProps + Send>;

pub struct RoonApi {
    reg_info: serde_json::Value,
    pairing: bool,
    paired_core: Arc<StdMutex<Option<Core>>>
}

#[derive(Clone, Debug)]
pub struct Core {
    pub display_name: String,
    pub display_version: String,
    core_id: String,
    moo_id: usize
}

impl RoonApi {
    pub fn new(options: serde_json::Value, pairing: bool) -> Self {
        let mut reg_info = options;

        reg_info["provided_services"] = json!([]);

        Self {
            reg_info,
            pairing,
            paired_core: Arc::new(StdMutex::new(None))
        }
    }

    pub async fn start_discovery(&self, on_core_found: fn(&Core), on_core_lost: fn(&Core), mut svcs: HashMap<String, Svc>) -> std::io::Result<Vec<JoinHandle<()>>> {
        const SERVICE_ID: &str = "00720724-5143-4a9b-abac-0e50cba674bb";
        const QUERY: [(&str, &str); 1] = [("query_service_id", SERVICE_ID)];
        let body = self.reg_info.clone();
        let sood_conns: Arc<TokioMutex<Vec<String>>> = Arc::new(TokioMutex::new(Vec::new()));
        let mut sood = Sood::new();
        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        let (handle, mut sood_rx) = sood.start().await?;
        let (moo_tx, mut moo_rx) = mpsc::channel::<Moo>(4);

        let paired_core = self.paired_core.clone();
        let query = async move {
            let mut scan_count = 0;

            loop {
                if scan_count < 6 || scan_count % 6 == 0 {
                    let paired_core = paired_core.lock().unwrap().to_owned();

                    if let None = paired_core {
                        if let Err(err) = sood.query(&QUERY).await {
                            println!("{}", err);
                        }
                    }
                }

                scan_count += 1;

                sleep(Duration::from_secs(10)).await;
            }
        };

        let sood_conns_clone = sood_conns.clone();
        let receive_response = async move {
            fn is_service_response(service_id: &str, msg: &mut Message) -> Option<(String, String)> {
                let svc_id = msg.props.remove("service_id")?;
                let unique_id = msg.props.remove("unique_id")?;
                let port = msg.props.remove("http_port")?;

                if msg.msg_type == 'R' && svc_id == service_id {
                    return Some((unique_id, port));
                }

                None
            }

            loop {
                if let Some(mut msg) = sood_rx.recv().await {
                    if let Some((unique_id, port)) = is_service_response(SERVICE_ID, &mut msg) {
                        let mut sood_conns = sood_conns.lock().await;

                        if !sood_conns.contains(&unique_id) {
                            println!("sood connection for unique_id: {}", unique_id);
                            sood_conns.push(unique_id.to_owned());

                            if let Ok(mut moo) = Moo::new(&msg.ip, &port, unique_id).await {
                                moo.send_request("com.roonlabs.registry:1/info", None).await.unwrap();
                                moo.receive_response().await.unwrap();
                                moo.send_request("com.roonlabs.registry:1/register", Some(&body)).await.unwrap();

                                moo_tx.send(moo).await.unwrap();
                            }
                        }
                    }
                }
            }
        };

        let pairing: bool = self.pairing;
        let paired_core = self.paired_core.clone();
        let on_moo_receive = async move {
            let mut moos: Vec<Moo> = Vec::new();
            let mut cores: Vec<Core> = Vec::new();
            let mut new_moo = None;
            let mut lost_moo = None;
            let mut mooid_msg: Option<(usize, serde_json::Value)> = None;
            let mut props_option: Option<RespProps> = None;
            let mut response_ids: Vec<(usize, usize)> = Vec::new();

            loop {
                if let Some(moo) = new_moo {
                    moos.push(moo);
                    new_moo = None;
                } else if let Some(index) = lost_moo {
                    let moo = moos.remove(index);

                    sood_conns_clone.lock().await.remove(index);
                    moo.clean_up();
                    lost_moo = None;
                } else if let Some((index, msg)) = mooid_msg {
                    let moo = moos.get_mut(index).unwrap();

                    if msg["verb"] == "REQUEST" {
                        let request_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                        let svc_name = msg["service"].as_str().unwrap().to_owned();
                        let svc = svcs.remove(&svc_name);

                        if let Some(svc) = svc {
                            let (hdr, body) = (svc.req_handler)(cores.get(index).unwrap(), Some(&msg));

                            if let Some(_) = hdr.get(2) {
                                let mut response_ids: Vec<(usize, usize)> = Vec::new();

                                {
                                    let sub_name = hdr[0];
                                    let sub_types = svc.sub_types.lock().unwrap();
    
                                    for (msg_key, msg) in sub_types.iter() {
                                        if msg_key.contains(sub_name) {
                                            let request_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                                            let moo_id = msg_key[msg_key.len()..].parse::<usize>().unwrap();
                                            response_ids.push((request_id, moo_id));
                                        }
                                    }
                                }

                                props_option = Some((&hdr[1..], body));
                            } else {
                                moo.send_response(request_id, &(hdr, body)).await.unwrap();
                            }

                            svcs.insert(svc_name, svc);
                        } else {
                            let error = format!("{}", msg["name"].as_str().unwrap());
                            let body = json!({"error" : error});
                            let props: RespProps = (&["COMPLETE", "InvalidRequest"], Some(body));

                            moo.send_response(request_id, &props).await.unwrap();
                        }
                    } else {
                        if !moo.handle_response() {

                        }
                    }

                    mooid_msg = None;
                }

                if let Some(props) = props_option {
                    for (request_id, moo_id) in response_ids {
                        let moo = moos.get_mut(moo_id).unwrap();
                        moo.send_response(request_id, &props).await.unwrap();
                    }

                    props_option = None;
                    response_ids = Vec::new();
                }

                if moos.len() == 0 {
                    new_moo = moo_rx.recv().await;
                } else {
                    let mut moo_receivers = Vec::new();

                    for moo in &mut moos.iter_mut() {
                        moo_receivers.push(moo.receive_response().boxed());
                    }

                    match select(moo_rx.recv().boxed(), select_all(moo_receivers)).await {
                        Either::Left((moo, _)) => {
                            new_moo = moo;
                        }
                        Either::Right(((Ok(msg), index, _), _)) => {
                            if msg["name"] == "Registered" {
                                let body = &msg["body"];
                                let core = Core {
                                    display_name: body["display_name"].as_str().unwrap().to_string(),
                                    display_version: body["display_version"].as_str().unwrap().to_string(),
                                    core_id: body["core_id"].as_str().unwrap().to_string(),
                                    moo_id: index
                                };

                                if pairing {
                                    let mut paired_core_id = None;

                                    {
                                        let mut paired_core = paired_core.lock().unwrap();

                                        if let None = *paired_core {
                                            *paired_core = Some(core.to_owned());
                                            paired_core_id = Some(core.core_id.to_owned());
                                        }
                                    }

                                    if let Some(_) = paired_core_id {
                                        let svc_name = "com.roonlabs.pairing:1";
                                        let svc = svcs.remove(svc_name);

                                        if let Some(svc) = svc {

                                            {
                                                let sub_name = "subscribe_pairing";
                                                let sub_types = svc.sub_types.lock().unwrap();
                
                                                for (msg_key, msg) in sub_types.iter() {
                                                    if msg_key.contains(sub_name) {
                                                        let request_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                                                        let moo_id = msg_key[msg_key.len()..].parse::<usize>().unwrap();
                                                        response_ids.push((request_id, moo_id));
                                                    }
                                                }
                                            }

                                            svcs.insert(svc_name.to_owned(), svc);

                                            let body = json!({"paired_core_id": core.core_id});
                                            props_option = Some((&["CONTINUE", "Changed"], Some(body)));
                                        }                
                                    }

                                    if let Some(paired_core_id) = paired_core_id {
                                        if core.core_id == paired_core_id {
                                            on_core_found(&core);
                                        }
                                    }
                                } else {
                                    on_core_found(&core);
                                }

                                cores.push(core);
                            } else {
                                mooid_msg = Some((index, msg));
                            }
                        }
                        Either::Right(((Err(_), index, _), _)) => {
                            let core = cores.remove(index);

                            lost_moo = Some(index);
                            on_core_lost(&core);
                        }
                    }
                }
            }
        };

        handles.push(handle);
        handles.push(tokio::spawn(query));
        handles.push(tokio::spawn(receive_response));
        handles.push(tokio::spawn(on_moo_receive));

        Ok(handles)
    }

    pub fn init_services(&mut self) -> HashMap<String, Svc> {
        let mut svcs: HashMap<String, Svc> = HashMap::new();

        self.add_ping_service(&mut svcs);

        if self.pairing {
            self.add_pairing_service(&mut svcs);
        }

        for (name, _) in &svcs {
            self.reg_info["provided_services"].as_array_mut().unwrap().push(json!(name));
        }

        svcs
    }

    pub fn register_service(&mut self, spec: SvcSpec) -> Svc
    {
        let mut methods = spec.methods;
        let sub_types = Arc::new(StdMutex::new(HashMap::new()));
        let mut sub_names = Vec::new();

        for sub in spec.subs {
            let sub_types_clone = sub_types.clone();
            let sub_name = sub.subscribe_name.clone();
            let sub_method = move |core: &Core, msg: Option<&serde_json::Value>| -> RespProps {
                let mut sub_types = sub_types_clone.lock().unwrap();

                if let Some(msg) = msg {
                    let sub_key = msg["body"]["subscription_key"].as_str().unwrap();
                    let msg_key = format!("{}:{}:{}", sub_name, core.moo_id, sub_key);

                    sub_types.insert(msg_key, msg.to_owned());
                }

                (sub.start)(core, msg)
            };

            let sub_types = sub_types.clone();
            let sub_name = sub.subscribe_name.clone();
            let unsub_method = move |core: &Core, msg: Option<&serde_json::Value>| -> RespProps {
                let mut sub_types = sub_types.lock().unwrap();
                let sub_mooid = format!("{}{}", sub_name, core.moo_id);

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
        let req_handler = move |core: &Core, req: Option<&serde_json::Value>| {
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
                        let sub_mooid = format!("{}{}", sub_name, core.moo_id);
                        sub_types.remove(&sub_mooid);
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
        let ping = |_: &Core, _: Option<&serde_json::Value>| -> RespProps {
            (&["COMPLETE", "Success"], None)
        };

        spec.add_method("ping", Box::new(ping));
        svcs.insert("com.roonlabs.ping:1".to_owned(), self.register_service(spec));
    }
 
    fn add_pairing_service(&mut self, svcs: &mut HashMap<String, Svc>) {
        let mut spec = SvcSpec::new();
        let paired_core = self.paired_core.clone();
        let get_pairing = move |_: &Core, _: Option<&serde_json::Value>| -> RespProps {
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
        let pair = move |core: &Core, _: Option<&serde_json::Value>| -> RespProps {
            let mut paired_core = paired_core.lock().unwrap();

            if let Some(paired_core) = paired_core.as_ref() {
                if paired_core.core_id == core.core_id {
                    println!("pair: {}", core.core_id);
                    return (&[], None)
                }
            }

            let body = json!({"paired_core_id": core.core_id});

            *paired_core = Some(core.to_owned());

            (&["subscribe_pairing", "CONTINUE", "Changed"], Some(body))
        };

        spec.add_method("pair", Box::new(pair));

        let paired_core = self.paired_core.clone();
        let start = move |_: &Core, _: Option<&serde_json::Value>| -> RespProps {
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
}

struct Sub {
    subscribe_name: String,
    unsubscribe_name: String,
    start: Method,
    end: Option<Method>
}

pub struct SvcSpec {
    methods: HashMap<String, Method>,
    subs: Vec<Sub>
}

impl SvcSpec {
    fn new() -> Self {
        Self {
            methods: HashMap::new(),
            subs: Vec::new()
        }
    }

    fn add_method(&mut self, name: &str, method: Method) {
        self.methods.insert(name.to_owned(), method);
    }

    fn add_sub(&mut self, sub: Sub) {
        self.subs.push(sub);
    }
}

pub struct Svc {
    sub_types: Arc<StdMutex<HashMap<String, serde_json::Value>>>,
    req_handler: Method
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn it_works() {
        let info = json!({
            "extension_id": "com.theappgineer.rust-roon-api",
            "display_name": "Rust Roon API",
            "display_version": "0.1.0",
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let mut roon = RoonApi::new(info, true);
        let on_core_found = move |core: &Core| {
            println!("Core found: {}, version {}", core.display_name, core.display_version);
        };
        let on_core_lost = move |core: &Core| {
            println!("Core lost: {}", core.display_name);
        };
        let svcs = roon.init_services();

        for handle in roon.start_discovery(on_core_found, on_core_lost, svcs).await.unwrap() {
            handle.await.unwrap();
        }
    }
}
