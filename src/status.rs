use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde_json::json;

use crate::{RoonApi, Core, Moo, RespProps, Sub, Svc, SvcSpec};

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
        let get_status = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            let (message, is_error) = &*props_clone.lock().unwrap();
            let body = json!({
                "message": message,
                "is_error": is_error
            });

            (&["COMPLETE", "Success"], Some(body))
        };

        spec.add_method("get_status", Box::new(get_status));

        let props_clone = props.clone();
        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            let (message, is_error) = &*props_clone.lock().unwrap();
            let body = json!({
                "message": message,
                "is_error": is_error
            });

            (&["CONTINUE", "Subscribed"], Some(body))
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

    pub async fn set_status(&self, message: String, is_error: bool) {
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

            if let Some(request_id) = response_ids.get(&moo.id) {
                let msg_string = Moo::create_msg_string(*request_id, &hdr, Some(&body));
                moo.send_msg_string(msg_string).await.unwrap();
            }
        }

        let mut props = self.props.lock().unwrap();

        *props = (message, is_error);
    }
}

#[cfg(test)]
#[cfg(feature = "status")]
mod tests {
    use std::collections::HashMap;
    use serde_json::json;

    use crate::status::{self, Status};
    use crate::{CoreEvent, Services, ROON_API_VERSION};

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
        let (svc, status) = Status::new(&roon);
        let services = vec![Services::Status(status)];
        let mut provided: HashMap<String, Svc> = HashMap::new();

        provided.insert(status::SVCNAME.to_owned(), svc);

        let (mut handles, mut core_rx) = roon.start_discovery(provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            loop {
                if let Some((core, _)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(mut core) => {
                            let message = format!("Core found: {}, version {}", core.display_name, core.display_version);

                            if let Some(status) = core.get_status() {
                                status.set_status(message, false).await;
                            };
                        }
                        CoreEvent::Lost(core) => {
                            let message = format!("Core lost: {}, version {}", core.display_name, core.display_version);

                            if let Some(status) = core.get_status() {
                                status.set_status(message, false).await;
                            };
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
