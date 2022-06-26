pub mod logger;
pub mod sood;
pub mod moo;
pub mod transport_websocket;
pub mod status;

use std::collections::HashMap;
use std::net::IpAddr;
use std::thread::{self, JoinHandle};
use std::sync::{Arc, Mutex};

use json::{JsonValue, object, array};
use tungstenite::Error;

use moo::Moo;
use transport_websocket::Transport;

use crate::logger::Logger;
use crate::moo::{MooMsg, Core};

const SERVICE_ID: &str = "00720724-5143-4a9b-abac-0e50cba674bb";
const TEN_SECONDS: i32 = 10 * 100;

pub struct RoonApi {
    logger: Arc<Logger>,
    _extension_opts: JsonValue,
    extension_reginfo: JsonValue,
    service_request_handlers: Arc<Mutex<HashMap<String, Box<dyn FnMut(Option<&mut MooMsg>, u32) + Send + 'static>>>>,
    on_core_found: Option<Arc<Mutex<Box<dyn FnMut(&Core) + Send + 'static>>>>,
    on_core_lost: Option<Arc<Mutex<Box<dyn FnMut(&Core) + Send + 'static>>>>
}

impl RoonApi {
    pub fn new(extension_opts: JsonValue) -> Self
    {
        let extension_reginfo = object! {
            extension_id:      extension_opts["extension_id"].clone(),
            display_name:      extension_opts["display_name"].clone(),
            display_version:   extension_opts["display_version"].clone(),
            publisher:         extension_opts["publisher"].clone(),
            email:             extension_opts["email"].clone(),
            required_services: array![],
            optional_services: array![],
            provided_services: array![]
        };

        Self {
            logger: Arc::new(Logger::new(extension_opts["log_level"].to_string())),
            _extension_opts: extension_opts,
            extension_reginfo,
            service_request_handlers: Arc::new(Mutex::new(HashMap::new())),
            on_core_found: None,
            on_core_lost: None
        }
    }

    pub fn start_discovery<F, G>(&'static mut self, on_core_found: F, on_core_lost: G) -> JoinHandle<()>
    where F: FnMut(&Core) + Send + 'static,
          G: FnMut(&Core) + Send + 'static
    {
        self.logger.log("start_discovery");
        self.on_core_found = Some(Arc::new(Mutex::new(Box::new(on_core_found))));
        self.on_core_lost = Some(Arc::new(Mutex::new(Box::new(on_core_lost))));

        let logger = self.logger.clone();
        let mut sood = sood::Sood::new(logger).unwrap();

        let handle = thread::spawn(move || {
            const QUERY: [(&str, &str); 1] = [("query_service_id", SERVICE_ID)];
            let mut scan_count = None;
            let on_startup = |sood: &sood::Sood| {
                scan_count = Some(0);
                sood.query(HashMap::from(QUERY));
            };
            let (_, rx) = sood.start(on_startup);
            let mut sood_conns: HashMap<String, bool> = HashMap::new();
            let mut loop_count = 0;
            let mut moos = Vec::new();

            loop {
                if let Ok(msg) = rx.try_recv() {
                    if let Some(service_id) = msg.props.get("service_id") {
                        if let Some(unique_id) = msg.props.get("unique_id") {
                            if let Some(port) = msg.props.get("http_port") {
                                if service_id == SERVICE_ID && !sood_conns.contains_key(unique_id) {
                                    sood_conns.insert(unique_id.clone(), true);
    
                                    self.logger.log(format!("sood connection for unique_id: {}", unique_id).as_str());
    
                                    moos.push(self.ws_connect(msg.from.ip(), port, unique_id));
                                }
                            }
                        }
                    }
                }

                let mut lost_moos = Vec::new();

                for mut moo in &mut moos {
                    match moo.receive_response() {
                        Err(error) => {
                            if error.is::<tungstenite::Error>() {
                                let error = *error.downcast::<tungstenite::Error>().unwrap();
    
                                if let tungstenite::Error::Io(error) = error {
                                    if error.kind() != std::io::ErrorKind::WouldBlock {
                                        panic!("{}", error);
                                    }
                                } else {
                                    let mut handlers = self.service_request_handlers.lock().unwrap();

                                    for (_, cb) in handlers.iter_mut() {
                                        (cb)(None, moo.mooid);
                                    }

                                    // Mark for later clean up
                                    lost_moos.push(moo.mooid);
                                }
                            } else {
                                panic!("{}", error);
                            }
                        }
                        Ok(msg) => {
                            if msg["verb"] == "REQUEST" {
                                let mut handlers = self.service_request_handlers.lock().unwrap();
                                let service = msg["service"].to_string();

                                if let Some(handler) = handlers.get_mut(&service) {
                                    let mooid = moo.mooid;
                                    let mut req = MooMsg::new(&mut moo, msg);

                                    (handler)(Some(&mut req), mooid);
                                }
                            } else {
                                moo.handle_response(&msg).unwrap();
                            }
                        }
                    }
                }

                // Perform clean up
                for mooid in lost_moos {
                    let mooid = mooid as usize;
                    let moo = &mut moos[mooid];

                    sood_conns.remove(&moo.unique_id);

                    moo.clean_up();
                    moos.remove(mooid);
                }

                if loop_count % TEN_SECONDS == 0 {
                    if let Some(count) = scan_count {
                        if count < 6 || count % 6 == 0 {
                            sood.query(HashMap::from(QUERY));
                        }

                        scan_count = Some(count + 1);
                    }
                }

                loop_count += 1;
    
                std::thread::sleep(core::time::Duration::from_millis(10));
            }
        });

        handle
    }

    pub fn init_services(&mut self, svcs: &[Svc]) {
        let ping = |req: &mut MooMsg| {
            (req.send_complete.lock().unwrap())("Success", None).unwrap();
        };
        let mut spec = SvcSpec::new();

        spec.add_method("ping".to_string(), ping);

        let service = self.register_service("com.roonlabs.ping:1", spec);
        let mut svcs = svcs.to_vec();

        svcs.push(service);

        for svc in svcs {
            self.extension_reginfo["provided_services"].push(svc.name).unwrap();
        }
    }

    pub fn register_service(&self, svc_name: &str, mut spec: SvcSpec) -> Svc {
        let subtypes = Arc::new(Mutex::new(HashMap::new()));

        if !spec.subs.is_empty() {
            for sub_mutex in &spec.subs[..] {
                let sub = sub_mutex.lock().unwrap();

                {
                    let clone = sub_mutex.clone();
                    let subname = sub.subscribe_name.clone();
                    let subtypes = subtypes.clone();

                    let subscribe = move |req: &mut MooMsg| {
                        let sub = clone.lock().unwrap();

                        (sub.start)(req);

                        let subscription_key = req.msg["body"]["subscription_key"].to_string();
                        let key = format!("{}/{}", subname, req.mooid);

                        // This assumes there is only one active subscription_key per subname/mooid combination
                        subtypes.lock().unwrap().insert(key, (subscription_key, req.clone()));
                    };

                    spec.methods.insert(sub.subscribe_name.to_owned(), Box::new(subscribe));
                }
                {
                    let clone = sub_mutex.clone();

                    let unsubscribe = move |req: &mut MooMsg| {
                        let sub = clone.lock().unwrap();

                        if let Some(end) = &sub.end {
                            (end)(Some(req));
                        }

                        (req.send_complete.lock().unwrap())("Unsubscribed", None).unwrap();
                    };

                    spec.methods.insert(sub.unsubscribe_name.to_owned(), Box::new(unsubscribe));
                }
            }
        }

        let clone = subtypes.clone();
        let handler = move |req: Option<&mut MooMsg>, mooid: u32| {
            if let Some(req) = req {
                let cb = spec.methods.get_mut(&req.msg["name"].to_string()).unwrap();

                (cb)(req);
            } else if !spec.subs.is_empty() {
                for sub in &spec.subs {
                    let sub = sub.lock().unwrap();
                    let subname = &sub.subscribe_name;
                    let key = format!("{}/{}", subname, mooid);

                    clone.lock().unwrap().remove(&key);

                    if let Some(end) = &sub.end {
                        (end)(None);
                    }
                }
            }
        };

        self.service_request_handlers.lock().unwrap().insert(svc_name.to_string(), Box::new(handler));

        let clone = subtypes.clone();
        let send_continue_all = move |subtype: &str, name: &str, props: JsonValue| -> Result<(), Error> {
            let subtypes = clone.lock().unwrap();

            for (key, (_, req)) in subtypes.iter() {
                let req_key = format!("{}/{}", subtype, req.mooid);

                if *key == req_key {
                    let mut send_continue = req.send_continue.lock().unwrap();

                    (send_continue)(name, Some(&props))?;
                }
            }

            Ok(())
        };

        let clone = subtypes.clone();
        let send_complete_all = move |subtype: &str, name: &str, props: JsonValue| -> Result<(), Error> {
            let mut subtypes = clone.lock().unwrap();
            let mut matched_keys = Vec::new();

            for (key, (_, req)) in subtypes.iter() {
                let req_key = format!("{}/{}", subtype, req.mooid);

                if *key == req_key {
                    let mut send_complete = req.send_complete.lock().unwrap();

                    (send_complete)(name, Some(&props))?;

                    matched_keys.push(key.to_owned());
                }
            }

            for key in matched_keys.iter() {
                subtypes.remove(key);
            }

            Ok(())
        };

        Svc {
            name: svc_name.to_owned(),
            send_continue_all: Arc::new(Mutex::new(Box::new(send_continue_all))),
            _send_complete_all: Arc::new(Mutex::new(Box::new(send_complete_all))),
            _subtypes: subtypes
        }
    }

    fn ws_connect(&'static self, ip: IpAddr, port: &String, unique_id: &String) -> Moo {
        let transport = Transport::new(ip, port).unwrap();
        let mut moo = Moo::new(transport, unique_id.to_owned());
        let extension_reginfo = self.extension_reginfo.clone();

        let reg_cb = move |moo: &mut Moo, _msg: Option<&JsonValue>| {
            moo.send_request(
                "com.roonlabs.registry:1/register",
                Some(&extension_reginfo),
                move |moo, msg| {
                    self.ev_registered(moo, msg);
                }
            ).unwrap();
        };

        moo.send_request(
            "com.roonlabs.registry:1/info",
            None,
            reg_cb
        ).unwrap();

        moo
    }

    fn ev_registered(&self, moo: &mut Moo, msg: Option<&JsonValue>) {
        match msg {
            Some(msg) => {
                if msg["name"] == "Registered" {
                    let body = &msg["body"];
                    let core = Core {
                        core_id:         body["core_id"].to_string(),
                        display_name:    body["display_name"].to_string(),
                        display_version: body["display_version"].to_string()
                    };

                    if let Some(on_core_found) = &self.on_core_found {
                        let mut on_core_found = on_core_found.lock().unwrap();

                        (on_core_found)(&core);
                    }

                    moo.core = Some(core);
                }
            }
            None => {
                if let Some(on_core_lost) = &self.on_core_lost {
                    let mut on_core_lost = on_core_lost.lock().unwrap();

                    if let Some(core) = &moo.core {
                        (on_core_lost)(core);
                    }
                }

                moo.core = None;
            }
        }
    }
}

struct Sub {
    subscribe_name: String,
    unsubscribe_name: String,
    start: Box<dyn Fn(&mut MooMsg) + Send + 'static>,
    end: Option<Box<dyn Fn(Option<&mut MooMsg>) + Send + 'static>>
}

impl Sub {
    fn new<F>(subscribe_name: &str, unsubscribe_name: &str, cb: F) -> Self
    where F: Fn(&mut MooMsg) + Send + 'static
    {
        Self {
            subscribe_name: subscribe_name.to_string(),
            unsubscribe_name: unsubscribe_name.to_string(),
            start: Box::new(cb),
            end: None
        }
    }
}

pub struct SvcSpec {
    subs: Vec<Arc<Mutex<Sub>>>,
    methods: HashMap<String, Box<dyn FnMut(&mut MooMsg) + Send + 'static>>
}

impl SvcSpec {
    fn new() -> Self {
        Self {
            subs: Vec::new(),
            methods: HashMap::new()
        }
    }

    fn add_sub(&mut self, sub: Sub) {
        let sub = Arc::new(Mutex::new(sub));
        self.subs.push(sub);
    }

    fn add_method<F>(&mut self, method: String, cb: F)
    where F: Fn(&mut MooMsg) + Send + 'static
    {
        self.methods.insert(method, Box::new(cb));
    }
}

#[derive(Clone)]
pub struct Svc {
    name: String,
    send_continue_all: Arc<Mutex<Box<dyn Fn(&str, &str, JsonValue) -> Result<(), Error> + Send>>>,
    _send_complete_all: Arc<Mutex<Box<dyn Fn(&str, &str, JsonValue) -> Result<(), Error> + Send>>>,
    _subtypes: Arc<Mutex<HashMap<String, (String, MooMsg)>>>
}

#[cfg(test)]
mod tests {
    use json::{object, JsonValue};
    use std::sync::{Arc, Mutex};

    use crate::{RoonApi, Core};
    use crate::status::Status;

    #[test]
    fn it_works() {
        let ext_opts: JsonValue = object! {
            extension_id:    "com.theappgineer.rust-roon-api",
            display_name:    "Rust Roon API",
            display_version: "0.1.0",
            publisher:       "The Appgineer",
            email:           "theappgineer@gmail.com"
        };
        let roon_api = Box::new(RoonApi::new(ext_opts));
        // Leak the RoonApi instance to give it a 'static lifetime
        let roon_api: &'static mut RoonApi = Box::leak(roon_api);
        let svc_status = Arc::new(Mutex::new(Status::new(&roon_api)));

        roon_api.init_services(&[svc_status.lock().unwrap().svc.clone()]);

        let clone = svc_status.clone();
        let on_core_found = move |core: &Core| {
            let message = format!("Core found: {}", core.display_name);
            let mut svc_status = clone.lock().unwrap();
            svc_status.set_status(&message, false);
        };
        let on_core_lost = move |core: &Core| {
            let message = format!("Core lost: {}", core.display_name);
            let mut svc_status = svc_status.lock().unwrap();
            svc_status.set_status(&message, false);
        };
        let handle = roon_api.start_discovery(on_core_found, on_core_lost);

        handle.join().unwrap();
    }
}
