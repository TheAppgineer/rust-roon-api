use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde_json::json;

use crate::{RoonApi, Core, Moo, RespProps, Sub, Svc, SvcSpec, send_complete, send_continue};

pub const SVCNAME: &str = "com.roonlabs.status:1";

#[derive(Clone, Debug)]
pub struct Status {
    moo: Option<Moo>,
    sub_types: Arc<Mutex<HashMap<String, usize>>>,
    props: Arc<Mutex<(String, bool)>>
}

impl Status {
    pub fn new(roon: &RoonApi) -> (Svc, Self) {
        let mut spec = SvcSpec::new(SVCNAME);
        let props = Arc::new(Mutex::new((String::new(), false)));

        let props_clone = props.clone();
        let get_status = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            let (message, is_error) = &*props_clone.lock().unwrap();
            let body = json!({
                "message": message,
                "is_error": is_error
            });

            send_complete!("Success", Some(body))
        };

        spec.add_method("get_status", Box::new(get_status));

        let props_clone = props.clone();
        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            let (message, is_error) = &*props_clone.lock().unwrap();
            let body = json!({
                "message": message,
                "is_error": is_error
            });

            send_continue!("Subscribed", Some(body))
        };

        spec.add_sub(Sub {
            subscribe_name: "subscribe_status".to_owned(),
            unsubscribe_name: "unsubscribe_status".to_owned(),
            start: Box::new(start),
            end: None
        });

        let svc = roon.register_service(spec);
        let status = Self {
            moo: None,
            sub_types: svc.sub_types.clone(),
            props
        };

        (svc, status)
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn set_status(&self, message: String, is_error: bool) -> Option<()> {
        let mut result = None;

        if let Some(moo) = &self.moo {
            let hdr = ["CONTINUE", "Changed"];
            let body = json!({
                "message": message,
                "is_error": is_error
            });

            let sub_name = "subscribe_status";
            let mut response_ids: HashMap<usize, usize> = HashMap::new();

            for (msg_key, req_id) in self.sub_types.lock().unwrap().iter() {
                let split: Vec<&str> = msg_key.split(':').collect();

                if split[0] == sub_name {
                    let moo_id = split[1].parse::<usize>().unwrap();
                    response_ids.insert(moo_id, *req_id);
                }
            }

            if let Some(req_id) = response_ids.get(&moo.id) {
                let msg_string = Moo::create_msg_string(*req_id, &hdr, Some(&body));

                result = moo.send_msg_string(*req_id, msg_string).await.ok();
            }
        }

        let mut props = self.props.lock().unwrap();

        *props = (message, is_error);

        result
    }
}

#[cfg(test)]
#[cfg(feature = "status")]
mod tests {
    use std::collections::HashMap;

    use crate::status::{self, Status};
    use crate::{CoreEvent, Info, Parsed, Services, info};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";
        let info = info!("com.theappgineer", "Rust Roon API");

        simple_logging::log_to_stderr(log::LevelFilter::Info);

        let mut roon = RoonApi::new(info);
        let (svc, status) = Status::new(&roon);
        let services = vec![Services::Status(status)];
        let mut provided: HashMap<String, Svc> = HashMap::new();
        let get_roon_state = || {
            RoonApi::load_config(CONFIG_PATH, "roonstate")
        };

        provided.insert(status::SVCNAME.to_owned(), svc);

        let (mut handles, mut core_rx) = roon
            .start_discovery(Box::new(get_roon_state), provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(mut core) => {
                            let message = format!("Core found: {}, version {}", core.display_name, core.display_version);

                            if let Some(status) = core.get_status() {
                                status.set_status(message, false).await;
                            };
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
        }));

        for handle in handles {
            handle.await.unwrap();
        }
    }
}
