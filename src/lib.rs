use std::collections::HashMap;
use std::sync::Arc;
use futures_util::future::{select_all, select, Either};
use futures_util::FutureExt;
use moo::Moo;
use serde_json::json;
use sood::{Message, Sood};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

mod moo;
mod sood;

pub struct RoonApi {
    reg_info: serde_json::Value
}

#[derive(Debug)]
pub struct Core {
    pub core_id: String,
    pub display_name: String,
    pub display_version: String
}

impl RoonApi {
    pub fn new(options: serde_json::Value) -> Self {
        let mut reg_info = options;

        reg_info["provided_services"] = json!([]);

        Self {
            reg_info
        }
    }

    pub async fn start_discovery(&self, on_core_found: fn(&Core), on_core_lost: fn(&Core), svcs: HashMap<String, Svc>) -> std::io::Result<Vec<JoinHandle<()>>> {
        const SERVICE_ID: &str = "00720724-5143-4a9b-abac-0e50cba674bb";
        const QUERY: [(&str, &str); 1] = [("query_service_id", SERVICE_ID)];
        let body = self.reg_info.clone();
        let sood_conns: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut sood = Sood::new();
        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        let (handle, mut sood_rx) = sood.start().await?;
        let (moo_tx, mut moo_rx) = mpsc::channel::<Moo>(4);

        let query = async move {
            let mut scan_count = 0;

            loop {
                if scan_count < 6 || scan_count % 6 == 0 {
                    if let Err(err) = sood.query(&QUERY).await {
                        println!("{}", err);
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

        let on_moo_receive = async move {
            let mut moos: Vec<Moo> = Vec::new();
            let mut cores: Vec<Core> = Vec::new();
            let mut new_moo = None;
            let mut lost_moo = None;
            let mut mooid_msg:Option<(usize, serde_json::Value)> = None;

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
                        let svc = msg["service"].as_str().unwrap();
                        let method = msg["name"].as_str().unwrap();

                        if let Some(svc) = svcs.get(svc) {
                            if let Some(resp_props) = svc.methods.get(method) {
                                moo.send_response(msg, resp_props).await.unwrap();
                            }
                        }
                    } else {
                        if !moo.handle_response() {

                        }
                    }
                    mooid_msg = None;
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
                                    core_id: body["core_id"].as_str().unwrap().to_string(),
                                    display_name: body["display_name"].as_str().unwrap().to_string(),
                                    display_version: body["display_version"].as_str().unwrap().to_string()
                                };

                                on_core_found(&core);
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
        let mut spec = SvcSpec::new();

        spec.add_method("ping", "COMPLETE", "Success", None);

        let mut svcs: HashMap<String, Svc> = HashMap::new();
        svcs.insert("com.roonlabs.ping:1".to_owned(), self.register_service(spec));

        for (name, _) in &svcs {
            self.reg_info["provided_services"].as_array_mut().unwrap().push(json!(name));
        }

        svcs
    }

    fn register_service(&mut self, spec: SvcSpec) -> Svc {
        Svc {
            methods: spec.methods
        }
    }
}

struct SvcSpec {
    methods: HashMap<String, (String, String, Option<serde_json::Value>)>
}

impl SvcSpec {
    fn new() -> Self {
        Self {
            methods: HashMap::new()
        }
    }

    fn add_method(&mut self, name: &str, state: &str, result: &str, body: Option<serde_json::Value>)
    {
        self.methods.insert(name.to_owned(), (state.to_owned(), result.to_owned(), body));
    }
}

pub struct Svc {
    methods: HashMap<String, (String, String, Option<serde_json::Value>)>
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
        let mut roon = RoonApi::new(info);
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
