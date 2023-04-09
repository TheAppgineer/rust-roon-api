use serde_json::json;

use crate::{RoonApi, Core, RespProps, Sub, Svc, SvcSpec};

pub const SVCNAME: &str = "com.roonlabs.pairing:1";

pub struct Pairing;

impl Pairing {
    pub fn new(roon: &RoonApi, on_core_lost: Box<dyn Fn(String) + Send>) -> Svc {
        let mut spec = SvcSpec::new(SVCNAME);
        let paired_core = roon.paired_core.clone();
        let get_pairing = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            match paired_core.lock().unwrap().as_ref() {
                Some(paired_core) => {
                    let body = json!({"paired_core_id": paired_core.id});

                    (&["COMPLETE", "Success"], Some(body))
                }
                None => {
                    (&["COMPLETE", "Success"], None)
                }
            }
        };

        spec.add_method("get_pairing", Box::new(get_pairing));

        let paired_core = roon.paired_core.clone();
        let pair = move |core: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            if let Some(core) = core {
                let mut paired_core = paired_core.lock().unwrap();

                if let Some(paired_core) = paired_core.as_ref() {
                    if paired_core.id == core.id {
                        return (&[], None)
                    } else {
                        on_core_lost(paired_core.id.to_owned());
                    }
                }

                let mut settings = RoonApi::load_config("roonstate");

                settings["paired_core_id"] = core.id.clone().into();
                RoonApi::save_config("roonstate", settings).unwrap();

                *paired_core = Some(core.to_owned());

                let body = json!({"paired_core_id": core.id});

                (&["subscribe_pairing", "CONTINUE", "Changed"], Some(body))
            } else {
                (&[], None)
            }
        };

        spec.add_method("pair", Box::new(pair));

        let paired_core = roon.paired_core.clone();
        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            match &*paired_core.lock().unwrap() {
                Some(paired_core) => {
                    let body = json!({"paired_core_id": paired_core.id});

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

        roon.register_service(spec)
    }
}

#[cfg(test)]
#[cfg(all(feature = "pairing", not(any(feature = "transport", feature = "browse"))))]
mod tests {
    use std::collections::HashMap;
    use serde_json::json;

    use super::*;
    use crate::{ROON_API_VERSION, CoreEvent};

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
