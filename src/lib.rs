use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use futures_util::{Future, FutureExt, future::select_all};
use moo::{ContentType, Moo, MooReceiver, MooSender};
use sood::{Message, Sood};
use serde::Serialize;
use serde_json::json;
use tokio::select;
use tokio::sync::mpsc::{self, Receiver};
use tokio::task::JoinSet;
use tokio::time::{sleep, Duration};

pub mod moo;
mod sood;

#[cfg(feature = "status")]    pub mod status;
#[cfg(feature = "settings")]  pub mod settings;
#[cfg(feature = "pairing")]   pub mod pairing;
#[cfg(feature = "transport")] pub mod transport;
#[cfg(feature = "browse")]    pub mod browse;
#[cfg(feature = "image")]     pub mod image;

pub const ROON_API_VERSION: &str = env!("CARGO_PKG_VERSION");

pub type RespProps = (&'static[&'static str], Option<serde_json::Value>);

type EventReceiver = Receiver<(CoreEvent, Option<(serde_json::Value, Parsed)>)>;
type Method = Box<dyn Fn(Option<&Core>, Option<&serde_json::Value>) -> Vec<RespProps> + Send>;

pub struct RoonApi {
    reg_info: serde_json::Value,
    sood_conns: Arc<tokio::sync::Mutex<Vec<String>>>,
    #[cfg(feature = "pairing")]
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
    #[cfg(any(feature = "status", feature = "transport", feature = "browse", feature = "image"))]
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
        let services = self.services.as_mut()?;

        #[cfg(any(feature = "status", feature = "settings", feature = "browse", feature = "image"))]
        let transport = services.iter_mut()
            .find_map(|svc| {
                if let Services::Transport(transport) = svc {
                    Some(transport)
                } else {
                    None
                }
            })?;

        #[cfg(not(any(feature = "status", feature = "settings", feature = "browse", feature = "image")))]
        let Services::Transport(transport) = services.iter_mut().next()?;

        transport.set_moo(self.moo.clone());

        Some(transport)
    }

    #[cfg(feature = "browse")]
    pub fn get_browse(&mut self) -> Option<&browse::Browse> {
        let services = self.services.as_mut()?;

        #[cfg(any(feature = "status", feature = "settings", feature = "transport", feature = "image"))]
        let browse = services.iter_mut()
            .find_map(|svc| {
                if let Services::Browse(browse) = svc {
                    Some(browse)
                } else {
                    None
                }
            })?;

        #[cfg(not(any(feature = "status", feature = "settings", feature = "transport", feature = "image")))]
        let Services::Browse(browse) = services.iter_mut().next()?;

        browse.set_moo(self.moo.clone());

        Some(browse)
    }

    #[cfg(feature = "image")]
    pub fn get_image(&mut self) -> Option<&image::Image> {
        let services = self.services.as_mut()?;

        #[cfg(any(feature = "status", feature = "settings", feature = "transport", feature = "browse"))]
        let image = services.iter_mut()
            .find_map(|svc| {
                if let Services::Image(image) = svc {
                    Some(image)
                } else {
                    None
                }
            })?;

        #[cfg(not(any(feature = "status", feature = "settings", feature = "transport", feature = "browse")))]
        let Services::Image(image) = services.iter_mut().next()?;

        image.set_moo(self.moo.clone());

        Some(image)
    }

    #[cfg(feature = "status")]
    pub fn get_status(&mut self) -> Option<&status::Status> {
        let services = self.services.as_mut()?;

        #[cfg(any(feature = "browse", feature = "transport", feature = "settings"))]
        let status = services.iter_mut()
            .find_map(|svc| {
                if let Services::Status(status) = svc {
                    Some(status)
                } else {
                    None
                }
            })?;

        #[cfg(not(any(feature = "browse", feature = "transport", feature = "settings")))]
        let Services::Status(status) = services.iter_mut().next()?;

        status.set_moo(self.moo.clone());

        Some(status)
    }
}

impl RoonApi {
    pub fn new(info: Info) -> Self {
        let mut reg_info = info.serialize(serde_json::value::Serializer).unwrap();

        reg_info["provided_services"] = json!([]);
        reg_info["required_services"] = json!([]);

        Self {
            reg_info,
            sood_conns: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            #[cfg(feature = "pairing")]
            lost_core_id: Arc::new(Mutex::new(None)),
            #[cfg(feature = "pairing")]
            paired_core: Arc::new(Mutex::new(None))
        }
    }

    pub async fn ws_connect(
        &mut self,
        get_roon_state: Box<dyn Fn() -> serde_json::Value + Send>,
        provided: HashMap<String, Svc>,
        #[cfg(any(feature = "status", feature = "settings", feature = "transport", feature = "browse", feature = "image"))]
        services: Option<Vec<Services>>,
        ip: &IpAddr,
        port: &str,
    ) -> Option<(JoinSet<()>, EventReceiver)> {
        let (moo_tx, moo_rx) = mpsc::channel::<(Moo, MooSender, MooReceiver)>(4);

        match Moo::new(ip, port).await {
            Ok(moo_tuple) => {
                #[cfg(any(feature = "status", feature = "settings", feature = "transport", feature = "browse", feature = "image"))]
                let (moo_handle, core_rx) = self.start_moo_receiver(get_roon_state, provided, services, moo_rx);

                #[cfg(not(any(feature = "status", feature = "settings", feature = "transport", feature = "browse", feature = "image")))]
                let (moo_handle, core_rx) = self.start_moo_receiver(get_roon_state, provided, moo_rx);
                let mut handles = JoinSet::new();

                moo_tx.send(moo_tuple).await.unwrap();

                handles.spawn(moo_handle);

                Some((handles, core_rx))
            }
            Err(_) => {
                None
            }
        }
    }

    pub async fn start_discovery(
        &mut self,
        get_roon_state: Box<dyn Fn() -> serde_json::Value + Send>,
        provided: HashMap<String, Svc>,
        #[cfg(any(feature = "status", feature = "settings", feature = "transport", feature = "browse", feature = "image"))]
        services: Option<Vec<Services>>,
    ) -> Option<(JoinSet<()>, EventReceiver)> {
        let (moo_tx, moo_rx) = mpsc::channel::<(Moo, MooSender, MooReceiver)>(4);
        let mut handles = self.start_sood_receiver(moo_tx).await;

        #[cfg(any(feature = "status", feature = "settings", feature = "transport", feature = "browse", feature = "image"))]
        let (moo_handle, core_rx) = self.start_moo_receiver(get_roon_state, provided, services, moo_rx);

        #[cfg(not(any(feature = "status", feature = "settings", feature = "transport", feature = "browse", feature = "image")))]
        let (moo_handle, core_rx) = self.start_moo_receiver(get_roon_state, provided, moo_rx);

        handles.spawn(moo_handle);

        Some((handles, core_rx))
    }

    async fn start_sood_receiver(
        &self,
        moo_tx: mpsc::Sender<(Moo, MooSender, MooReceiver)>,
    ) -> JoinSet<()> {
        const SERVICE_ID: &str = "00720724-5143-4a9b-abac-0e50cba674bb";
        let mut sood = Sood::new();
        let (sood_handle, mut sood_rx) = sood.start().await.unwrap();

        self.sood_conns.lock().await.clear();

        #[cfg(feature = "pairing")]
        let paired_core = self.paired_core.clone();

        let query = async move {
            const QUERY: [(&str, &str); 1] = [("query_service_id", SERVICE_ID)];
            let mut scan_count = 0;

            loop {
                if scan_count < 6 || scan_count % 6 == 0 {
                    #[cfg(feature = "pairing")]
                    {
                        let paired_core = paired_core.lock().unwrap().to_owned();

                        if paired_core.is_none() {
                            if let Err(err) = sood.query(&QUERY).await {
                                log::error!("{}", err);
                                break;
                            }
                        }
                    }

                    #[cfg(not(feature = "pairing"))]
                    match sood.query(&QUERY).await {
                        Ok(()) => log::debug!("sood query sent"),
                        Err(err) => {
                            log::error!("{}", err);
                            break;
                        }
                    }
                }

                scan_count += 1;

                sleep(Duration::from_secs(10)).await;
            }
        };

        let moo_tx = moo_tx.clone();
        let sood_conns_clone = self.sood_conns.clone();
        let sood_receive = async move {
            fn is_service_response(service_id: &str, msg: &mut Message) -> Option<(String, String)> {
                let svc_id = msg.props.remove("service_id")?;
                let unique_id = msg.props.remove("unique_id")?;
                let port = msg.props.remove("http_port")?;

                if msg.msg_type == 'R' && svc_id == service_id {
                    Some((unique_id, port))
                } else {
                    None
                }
            }

            loop {
                if let Some(mut msg) = sood_rx.recv().await {
                    if let Some((unique_id, port)) = is_service_response(SERVICE_ID, &mut msg) {
                        let mut sood_conns = sood_conns_clone.lock().await;

                        log::debug!("sood received: {}", unique_id);

                        if !sood_conns.contains(&unique_id) {
                            sood_conns.push(unique_id.to_owned());

                            if let Ok(moo_tuple) = Moo::new(&msg.ip, &port).await {
                                moo_tx.send(moo_tuple).await.unwrap();
                            }
                        }
                    }
                }
            }
        };

        let mut handles = JoinSet::new();

        handles.spawn(sood_handle);
        handles.spawn(query);
        handles.spawn(sood_receive);

        handles
    }

    fn start_moo_receiver(
        &self,
        get_roon_state: Box<dyn Fn() -> serde_json::Value + Send>,
        mut provided: HashMap<String, Svc>,
        #[cfg(any(feature = "status", feature = "settings", feature = "transport", feature = "browse", feature = "image"))]
        services: Option<Vec<Services>>,
        mut moo_rx: mpsc::Receiver<(Moo, MooSender, MooReceiver)>,
    ) -> (impl Future<Output = ()>, EventReceiver) {
        let (core_tx, core_rx) = mpsc::channel::<(CoreEvent, Option<(serde_json::Value, Parsed)>)>(4);

        let ping = Ping::create(self);

        provided.insert(ping.name.to_owned(), ping);

        #[cfg(feature = "pairing")]
        {
            let mut lost_core_id = self.lost_core_id.lock().unwrap();
            let mut paired_core = self.paired_core.lock().unwrap();

            *lost_core_id = None;
            *paired_core = None;

            let lost_core_id = self.lost_core_id.clone();
            let on_core_lost = move |core_id: String| {
                let mut lost_core_id = lost_core_id.lock().unwrap();
                *lost_core_id = Some(core_id);
            };
            let pairing = pairing::Pairing::create(self, Box::new(on_core_lost));

            provided.insert(pairing.name.to_owned(), pairing);
        }

        #[cfg(feature = "pairing")]
        let paired_core = self.paired_core.clone();

        let sood_conns = self.sood_conns.clone();
        let mut reg_info = self.reg_info.clone();
        let moo_receive = async move {
            let mut moo_senders: Vec<MooSender> = Vec::new();
            let mut cores: HashMap<usize, Core> = HashMap::new();
            let mut user_moos: HashMap<usize, Moo> = HashMap::new();
            let mut moo_receivers: Vec<MooReceiver> = Vec::new();

            'moo: loop {
                let mut msg_receivers = Vec::new();
                let mut req_receivers = Vec::new();
                let mut new_moo = None;
                let mut index_msg: Option<(usize, Option<(serde_json::Value, ContentType)>)> = None;
                let mut props_option: Vec<RespProps> = Vec::new();
                let mut response_ids: Vec<HashMap<usize, usize>> = Vec::new();
                let mut msg_string: Option<String> = None;

                if moo_receivers.is_empty() {
                    if let Some(mut moo) = moo_rx.recv().await {
                        let (_, moo_sender, _) = &mut moo;

                        moo_sender.send_req("com.roonlabs.registry:1/info", None).await.unwrap();
                        new_moo = Some(moo);
                    }
                } else {
                    for moo in moo_receivers.iter_mut() {
                        msg_receivers.push(moo.receive_response().boxed());
                    }

                    for moo in &mut moo_senders.iter_mut() {
                        req_receivers.push(moo.msg_rx.recv().boxed());
                    }

                    select! {
                        Some(mut moo) = moo_rx.recv() => {
                            let (_, moo_sender, _) = &mut moo;
                            moo_sender.send_req("com.roonlabs.registry:1/info", None).await.unwrap();
                            new_moo = Some(moo);
                        }
                        (msg, index, _) = select_all(msg_receivers) => {
                            index_msg = Some((index, msg.ok()));
                        }
                        (Some((req_id, raw_msg)), index, _) = select_all(req_receivers) => {
                            msg_string = Some(raw_msg);
                            response_ids.push(HashMap::from([(index, req_id)]));
                        }
                    }
                }

                if let Some((moo, moo_sender, moo_receiver)) = new_moo {
                    moo_receivers.push(moo_receiver);
                    user_moos.insert(moo_senders.len(), moo);
                    moo_senders.push(moo_sender);

                    // Restart loop to start using
                    continue;
                } else if let Some((index, msg)) = index_msg {
                    match msg {
                        Some((msg, _body)) => {
                            if msg["request_id"] == "0" && msg["body"]["core_id"].is_string() {
                                let roon_state = get_roon_state();
                                let moo = moo_senders.get_mut(index).unwrap();

                                #[cfg(any(feature = "transport", feature = "browse", feature = "image"))]
                                if let Some(services) = &services {
                                    for svc in services {
                                        match svc {
                                            #[cfg(feature = "transport")]
                                            Services::Transport(_) => {
                                                reg_info["required_services"].as_array_mut().unwrap().push(json!(transport::SVCNAME));
                                            }
                                            #[cfg(feature = "browse")]
                                            Services::Browse(_) => {
                                                reg_info["required_services"].as_array_mut().unwrap().push(json!(browse::SVCNAME));
                                            }
                                            #[cfg(feature = "image")]
                                            Services::Image(_) => {
                                                reg_info["required_services"].as_array_mut().unwrap().push(json!(image::SVCNAME));
                                            }
                                            #[cfg(any(feature = "status", feature = "settings"))]
                                            _ => ()
                                        }
                                    }
                                }

                                for name in provided.keys() {
                                    reg_info["provided_services"].as_array_mut().unwrap().push(json!(name));
                                }

                                if let Some(tokens) = roon_state.get("tokens") {
                                    let core_id = msg["body"]["core_id"].as_str().unwrap();

                                    if let Some(token) = tokens.get(core_id) {
                                        reg_info["token"] = token.to_owned();
                                    }
                                }

                                let req_id = moo.req_id.lock().unwrap().to_owned();

                                props_option.push((&["REQUEST", "com.roonlabs.registry:1/register"], Some(reg_info.to_owned())));
                                response_ids.push(HashMap::from([(index, req_id)]));
                            } else if msg["name"] == "Registered" {
                                let body = &msg["body"];
                                let moo = user_moos.remove(&index).unwrap();
                                let core = Core {
                                    display_name: body["display_name"].as_str().unwrap().to_string(),
                                    display_version: body["display_version"].as_str().unwrap().to_string(),
                                    id: body["core_id"].as_str().unwrap().to_string(),
                                    moo,
                                    #[cfg(any(feature = "status", feature = "transport", feature = "browse", feature = "image"))]
                                    services: services.clone()
                                };
                                let mut roon_state = get_roon_state();

                                roon_state["tokens"][&core.id] = body["token"].as_str().unwrap().into();

                                #[cfg(feature = "pairing")]
                                {
                                    let mut paired_core_id = None;

                                    {
                                        let mut paired_core = paired_core.lock().unwrap();

                                        if paired_core.is_none() {
                                            if let Some(set_core_id) = roon_state.get("paired_core_id") {
                                                if set_core_id.as_str().unwrap() == core.id {
                                                    *paired_core = Some(core.to_owned());
                                                    paired_core_id = Some(core.id.to_owned());
                                                }
                                            } else {
                                                *paired_core = Some(core.to_owned());
                                                paired_core_id = Some(core.id.to_owned());
                                                roon_state["paired_core_id"] = core.id.to_owned().into();
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
                                                        let moo_id_str = msg_key.split(':').nth(1).unwrap();
                                                        let moo_id = moo_id_str.parse::<usize>().unwrap();
                                                        let index = moo_senders.iter().position(|moo| moo.id == moo_id);

                                                        if let Some(index) = index {
                                                            response_ids.push(HashMap::from([(index, *req_id)]));
                                                        }
                                                    }
                                                }
                                            }

                                            provided.insert(svc_name.to_owned(), svc);

                                            let body = json!({"paired_core_id": paired_core_id});
                                            send_continue!(props_option, "Changed", Some(body));
                                        }
                                    }
                                }

                                core_tx.send((CoreEvent::Found(core.clone()), Some((roon_state, Parsed::RoonState)))).await.unwrap();

                                cores.insert(core.moo.id, core);
                            } else if msg["verb"] == "REQUEST" {
                                let request_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
                                let svc_name = msg["service"].as_str().unwrap().to_owned();
                                let svc = provided.remove(&svc_name);

                                if let Some(svc) = svc {
                                    let moo_id = moo_senders[index].id;
                                    let mut roon_state = None;

                                    for resp_props in (svc.req_handler)(cores.get(&moo_id), Some(&msg)) {
                                        let (hdr, body) = resp_props;

                                        if hdr.get(2).is_some() {
                                            let sub_name = hdr[0];

                                            if sub_name == "subscribe_pairing" {
                                                let mut state = get_roon_state();

                                                if let Some(paired_core_id) = body.as_ref().unwrap().get("paired_core_id") {
                                                    state["paired_core_id"] = paired_core_id.clone();
                                                    roon_state = Some(state);
                                                }
                                            }

                                            let sub_types = svc.sub_types.lock().unwrap();
                                            let mut resp_ids = HashMap::new();

                                            for (msg_key, req_id) in sub_types.iter() {
                                                let split: Vec<&str> = msg_key.split(':').collect();

                                                if split[0] == sub_name {
                                                    let index = split[1].parse::<usize>().unwrap();
                                                    resp_ids.insert(index, *req_id);
                                                }
                                            }

                                            response_ids.push(resp_ids);
                                            props_option.push((&hdr[1..], body));
                                        } else {
                                            props_option.push((hdr, body));
                                            response_ids.push(HashMap::from([(index, request_id)]));
                                        }
                                    }

                                    if let Some(roon_state) = roon_state {
                                        core_tx.send((CoreEvent::None, Some((roon_state, Parsed::RoonState)))).await.unwrap();
                                    }

                                    provided.insert(svc_name, svc);
                                } else {
                                    let error = msg["name"].as_str().unwrap();
                                    let body = json!({"error" : error});

                                    send_complete!(props_option, "InvalidRequest", Some(body));
                                    response_ids.push(HashMap::from([(index, request_id)]));
                                }
                            } else {
                                #[cfg(any(feature = "transport", feature = "browse", feature = "image"))]
                                if let Some(svcs) = services.as_ref() {
                                    for svc in svcs {
                                        match svc {
                                            #[cfg(feature = "transport")]
                                            Services::Transport(transport) => {
                                                if let Ok(mut parsed) = transport.parse_msg(&msg).await {
                                                    if let Some(item) = parsed.pop() {
                                                        core_tx.send((CoreEvent::None, Some((msg, item)))).await.unwrap();

                                                        while let Some(item) = parsed.pop() {
                                                            core_tx.send((CoreEvent::None, Some((serde_json::Value::Null, item)))).await.unwrap();
                                                        }

                                                        break;
                                                    }
                                                } else {
                                                    log::warn!("Failed to parse message: {}", msg);

                                                    core_tx.send((CoreEvent::None, Some((msg, Parsed::None)))).await.unwrap();
                                                    break;
                                                }
                                            }
                                            #[cfg(feature = "browse")]
                                            Services::Browse(browse) => {
                                                let parsed = browse.parse_msg(&msg).await;

                                                match parsed {
                                                    Parsed::None => (),
                                                    _ => {
                                                        core_tx.send((CoreEvent::None, Some((msg, parsed)))).await.unwrap();
                                                        break;
                                                    }
                                                }
                                            }
                                            #[cfg(feature = "image")]
                                            Services::Image(image) => {
                                                if let Some(parsed) = image.parse_msg(&msg, &_body).await {
                                                    core_tx.send((
                                                        CoreEvent::None,
                                                        Some((serde_json::Value::Null, parsed))
                                                    )).await.unwrap();
                                                }
                                            }
                                            #[cfg(any(feature = "status", feature = "settings"))]
                                            _ => ()
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            let mut sood_conns = sood_conns.lock().await;
                            let moo = moo_senders.remove(index);

                            moo_receivers.remove(index);

                            if index < sood_conns.len() {
                                sood_conns.remove(index);
                            }

                            if let Some(lost_core) = cores.remove(&moo.id) {
                                core_tx.send((CoreEvent::Lost(lost_core), None)).await.unwrap();
                            }

                            if moo_receivers.is_empty() {
                                break 'moo;
                            }
                        }
                    }
                }

                for (index, resp_ids) in response_ids.iter().enumerate() {
                    for (moo_index, req_id) in resp_ids {
                        if let Some(moo) = moo_senders.get_mut(*moo_index) {
                            let result = if let Some(msg_string) = &msg_string {
                                moo.send_msg_string(msg_string).await
                            } else {
                                let (hdr, body) = &props_option[index];

                                #[cfg(feature = "settings")]
                                if let Some(svcs) = services.as_ref() {
                                    for svc in svcs {
                                        match svc {
                                            Services::Settings(settings) => {
                                                let parsed = settings.parse_msg(req_id, body.as_ref());

                                                match parsed {
                                                    Parsed::None => (),
                                                    _ => {
                                                        core_tx.send((CoreEvent::None, Some((json!({}), parsed)))).await.unwrap();
                                                    }
                                                }
                                                #[cfg(any(feature = "status", feature = "browse", feature = "transport"))]
                                                break;
                                            }
                                            #[cfg(any(feature = "status", feature = "browse", feature = "transport"))]
                                            _ => ()
                                        }
                                    }
                                }

                                moo.send_msg(*req_id, hdr, body.as_ref()).await
                            };

                            if result.is_err() {
                                let index = *moo_index;
                                let mut sood_conns = sood_conns.lock().await;
                                let moo = moo_senders.remove(index);

                                moo_receivers.remove(index);

                                if index < sood_conns.len() {
                                    sood_conns.remove(index);
                                }

                                if let Some(lost_core) = cores.remove(&moo.id) {
                                    core_tx.send((CoreEvent::Lost(lost_core), None)).await.unwrap();
                                }

                                if moo_receivers.is_empty() {
                                    break 'moo;
                                }
                            }
                        }
                    }
                }
            }

            log::warn!("Terminating Moo");
        };

        (moo_receive, core_rx)
    }

    pub fn register_service(&self, spec: SvcSpec) -> Svc {
        let mut methods = spec.methods;
        let sub_types = Arc::new(Mutex::new(HashMap::new()));
        let mut sub_names = Vec::new();

        for sub in spec.subs {
            let sub_types_clone = sub_types.clone();
            let sub_name = sub.subscribe_name.clone();
            let sub_method = move |core: Option<&Core>, msg: Option<&serde_json::Value>| -> Vec<RespProps> {
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
            let unsub_method = move |core: Option<&Core>, msg: Option<&serde_json::Value>| -> Vec<RespProps> {
                let mut sub_types = sub_types.lock().unwrap();

                if let Some(msg) = msg {
                    let sub_key = msg["body"]["subscription_key"].as_str().unwrap();
                    let msg_key = format!("{}:{}:{}", sub_name, core.unwrap().moo.id, sub_key);

                    sub_types.remove(&msg_key);
                }

                if let Some(end) = &sub.end {
                    (end)(core, msg);
                }

                send_complete!("Unsubscribed", None)
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

            vec![(&[], None)]
        };

        Svc {
            name: spec.name,
            sub_types,
            req_handler: Box::new(req_handler)
        }
    }

    pub fn save_config(path: &str, key: &str, value: serde_json::Value) -> std::io::Result<()> {
        let mut config = match Self::read_and_parse(path) {
            Some(config) => config,
            None => json!({})
        };

        config[key] = value;

        std::fs::write(path, serde_json::to_string_pretty(&config).unwrap())
    }

    pub fn load_config(path: &str, key: &str) -> serde_json::Value {
        match Self::read_and_parse(path) {
            Some(value) => {
                match value.get(key) {
                    Some(value) => value.to_owned(),
                    None => json!({})
                }
            }
            None => json!({})
        }
    }

    fn read_and_parse(path: &str) -> Option<serde_json::Value> {
        let content = std::fs::read(path).ok()?;
        let content = std::str::from_utf8(&content).ok()?;

        serde_json::from_str::<serde_json::Value>(content).ok()
    }
}

struct Ping;

impl Ping {
    fn create(roon: &RoonApi) -> Svc {
        const SVCNAME: &str = "com.roonlabs.ping:1";
        let mut spec = SvcSpec::new(SVCNAME);
        let ping = |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            send_complete!("Success", None)
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
    #[cfg(feature = "status")]    Status(status::Status),
    #[cfg(feature = "settings")]  Settings(settings::Settings),
    #[cfg(feature = "transport")] Transport(transport::Transport),
    #[cfg(feature = "browse")]    Browse(browse::Browse),
    #[cfg(feature = "image")]     Image(image::Image),
}

#[derive(Debug)]
pub enum Parsed {
    None,
    RoonState,
    #[cfg(feature = "settings")]  SettingsSaved(serde_json::Value),
    #[cfg(feature = "transport")] Zones(Vec<transport::Zone>),
    #[cfg(feature = "transport")] ZonesSeek(Vec<transport::ZoneSeek>),
    #[cfg(feature = "transport")] ZonesRemoved(Vec<String>),
    #[cfg(feature = "transport")] Outputs(Vec<transport::Output>),
    #[cfg(feature = "transport")] OutputsRemoved(Vec<String>),
    #[cfg(feature = "transport")] Queue(Vec<transport::QueueItem>),
    #[cfg(feature = "transport")] QueueChanges(Vec<transport::QueueChange>),
    #[cfg(feature = "browse")]    BrowseResult(browse::BrowseResult, Option<String>),
    #[cfg(feature = "browse")]    LoadResult(browse::LoadResult, Option<String>),
    #[cfg(feature = "image")]     Jpeg((String, Vec<u8>)),
    #[cfg(feature = "image")]     Png((String, Vec<u8>)),
}

#[derive(Serialize)]
pub struct Info {
    extension_id: String,
    display_name: &'static str,
    display_version: &'static str,
    publisher: Option<&'static str>,
    email: &'static str,
    website: Option<&'static str>,
}

#[macro_export]
macro_rules! info {
    ($extension_id_prefix:literal, $display_name:literal) => {
        {
            let name = env!("CARGO_PKG_NAME");
            let authors = env!("CARGO_PKG_AUTHORS");
            let homepage = env!("CARGO_PKG_HOMEPAGE");
            let repository = env!("CARGO_PKG_REPOSITORY");
            let version = env!("CARGO_PKG_VERSION");

            let (publisher, email) = if authors.len() > 0 {
                let author_option = authors.split(':').next();

                if let Some(author) = author_option {
                    let start = author.find('<');

                    if let Some(start) = start {
                        let end = author.find('>').unwrap();

                        (Some(author[0..start].trim()), &author[start+1..end])
                    } else {
                        (author_option, "")
                    }
                } else {
                    (author_option, "")
                }
            } else {
                (None, "")
            };

            let website = if homepage.len() > 0 {
                Some(homepage)
            } else if repository.len() > 0 {
                Some(repository)
            } else {
                None
            };

            let mut extension_id = String::from($extension_id_prefix);

            extension_id.push('.');
            extension_id.push_str(name);

            Info::new(extension_id, $display_name, version, publisher, email, website)
        }
    };
}

impl Info {
    pub fn new(
        extension_id: String,
        display_name: &'static str,
        display_version: &'static str,
        publisher: Option<&'static str>,
        email: &'static str,
        website: Option<&'static str>
    ) -> Self {
        Self {
            extension_id,
            display_name,
            display_version,
            publisher,
            email,
            website,
        }
    }
}

#[cfg(test)]
#[cfg(not(any(feature = "pairing", feature = "status", feature = "settings", feature = "image")))]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";
        const LOG_FILE: &str = concat!(env!("CARGO_PKG_NAME"), ".log");

        simple_logging::log_to_file(LOG_FILE, log::LevelFilter::Debug).unwrap();

        let info = info!("com.theappgineer", "Rust Roon API");
        let mut roon = RoonApi::new(info);
        let provided: HashMap<String, Svc> = HashMap::new();
        let get_roon_state = || {
            RoonApi::load_config(CONFIG_PATH, "roonstate")
        };
        let (mut handles, mut core_rx) = roon
            .start_discovery(Box::new(get_roon_state), provided).await.unwrap();

        handles.spawn(async move {
            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(core) => {
                            log::info!("Core found: {}, version {}", core.display_name, core.display_version);
                        }
                        CoreEvent::Lost(core) => {
                            log::warn!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }

                    if let Some((msg, parsed)) = msg {
                        if let Parsed::RoonState = parsed {
                            RoonApi::save_config(CONFIG_PATH, "roonstate", msg).unwrap();
                        }
                    }
                }
            }
        });

        handles.join_next().await;
    }
}
