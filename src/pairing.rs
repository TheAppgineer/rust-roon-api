use serde_json::json;

use crate::{RoonApi, Core, RespProps, Sub, Svc, SvcSpec, send_complete, send_continue, send_continue_all};

pub const SVCNAME: &str = "com.roonlabs.pairing:1";

pub struct Pairing;

impl Pairing {
    pub fn create(roon: &RoonApi, on_core_lost: Box<dyn Fn(String) + Send>) -> Svc {
        let mut spec = SvcSpec::new(SVCNAME);
        let paired_core = roon.paired_core.clone();
        let get_pairing = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            match paired_core.lock().unwrap().as_ref() {
                Some(paired_core) => {
                    let body = json!({"paired_core_id": paired_core.id});

                    send_complete!("Success", Some(body))
                }
                None => {
                    send_complete!("Success", None)
                }
            }
        };

        spec.add_method("get_pairing", Box::new(get_pairing));

        let paired_core = roon.paired_core.clone();
        let pair = move |core: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            if let Some(core) = core {
                let mut paired_core = paired_core.lock().unwrap();

                if let Some(paired_core) = paired_core.as_ref() {
                    if paired_core.id == core.id {
                        return vec![(&[], None)]
                    } else {
                        on_core_lost(paired_core.id.to_owned());
                    }
                }

                *paired_core = Some(core.to_owned());

                let body = json!({"paired_core_id": core.id});

                send_continue_all!("subscribe_pairing", "Changed", Some(body))
            } else {
                vec![(&[], None)]
            }
        };

        spec.add_method("pair", Box::new(pair));

        let paired_core = roon.paired_core.clone();
        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            match &*paired_core.lock().unwrap() {
                Some(paired_core) => {
                    let body = json!({"paired_core_id": paired_core.id});

                    send_continue!("Subscribed", Some(body))
                }
                None => {
                    let body = json!({"paired_core_id": "undefined"});

                    send_continue!("Subscribed", Some(body))
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

    use super::*;
    use crate::{CoreEvent, Info, Parsed, info};

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";

        simple_logging::log_to_stderr(log::LevelFilter::Info);

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
                        _ => (),
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
