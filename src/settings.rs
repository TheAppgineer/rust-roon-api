use serde_json::json;

use crate::{RoonApi, Core, RespProps, Sub, Svc, SvcSpec};

pub const SVCNAME: &str = "com.roonlabs.settings:1";

#[derive(Clone, Debug)]
pub struct Settings;

impl Settings {
    pub fn new(
        roon: &RoonApi,
        get_settings_cb: fn(fn(serde_json::Value) -> Vec<RespProps>) -> Vec<RespProps>,
        save_settings_cb: fn(bool, serde_json::Value) -> Vec<RespProps>
    ) -> Svc {
        let mut spec = SvcSpec::new(SVCNAME);

        let get_settings = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            get_settings_cb(|settings| {
                vec![(&["COMPLETE", "Success"], Some(json!({"settings": settings})))]
            })
        };

        spec.add_method("get_settings", Box::new(get_settings));

        let save_settings = move |_: Option<&Core>, req: Option<&serde_json::Value>| -> Vec<RespProps> {
            if let Some(req) = req {
                let is_dry_run = req["body"]["is_dry_run"].as_bool().unwrap();
                let settings = &req["body"]["settings"];

                save_settings_cb(is_dry_run, settings.to_owned())
            } else {
                vec![(&[], None)]
            }
        };

        spec.add_method("save_settings", Box::new(save_settings));

        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            get_settings_cb(|settings| {
                vec![(&["CONTINUE", "Subscribed"], Some(json!({"settings": settings})))]
            })
        };

        spec.add_sub(Sub {
            subscribe_name: "subscribe_settings".to_owned(),
            unsubscribe_name: "unsubscribe_settings".to_owned(),
            start: Box::new(start),
            end: None
        });

        roon.register_service(spec)
    }
}

#[cfg(test)]
#[cfg(feature = "settings")]
mod tests {
    use std::collections::HashMap;

    use crate::settings::{self, Settings};
    use crate::{CoreEvent, ROON_API_VERSION};

    use super::*;

    fn make_layout(settings: &serde_json::Value) -> serde_json::Value {
        json!({
            "values": settings,
            "layout": [{
                "type": "dropdown",
                "title": "Rust Settings API dropdown",
                "values": [
                    { "title": "Disabled", "value": false },
                    { "title": "Enabled", "value": true }
                ],
                "setting": "dropdown"
            }],
            "has_error": false
        })
    }

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
        let get_settings = |cb: fn(serde_json::Value) -> Vec<RespProps>| -> Vec<RespProps> {
            let my_settings = RoonApi::load_config("settings");
            let layout = make_layout(&my_settings);

            cb(layout)
        };
        let save_settings = |is_dry_run: bool, settings: serde_json::Value| -> Vec<RespProps> {
            let layout = make_layout(&settings["values"]);
            let mut resp_props: Vec<RespProps> = vec![(&["COMPLETE", "Success"], Some(json!({"settings": layout})))];

            if !is_dry_run && !layout["has_error"].as_bool().unwrap() {
                RoonApi::save_config("settings", layout["values"].to_owned()).unwrap();

                resp_props.push((&["subscribe_settings", "CONTINUE", "Changed"], Some(json!({"settings": layout}))));
            }

            resp_props
        };
        let svc = Settings::new(&roon, get_settings, save_settings);
        let mut provided: HashMap<String, Svc> = HashMap::new();

        provided.insert(settings::SVCNAME.to_owned(), svc);

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
