use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{RoonApi, Core, Parsed, RespProps, Sub, Svc, SvcSpec, send_complete, send_continue};

pub const SVCNAME: &str = "com.roonlabs.settings:1";

#[derive(Serialize)]
pub struct Dropdown {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub values: Vec<HashMap<&'static str, Value>>,
    pub setting: &'static str
}

#[derive(Serialize)]
pub struct Group {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub collapsable: bool,
    pub items: Vec<Widget>
}

#[derive(Serialize)]
pub struct Integer {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub min: Option<i32>,
    pub max: Option<i32>,
    pub setting: &'static str
}

#[derive(Serialize)]
pub struct Label {
    pub title: &'static str,
    pub subtitle: Option<String>
}

#[derive(Serialize)]
pub struct OutputDropdown {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub setting: &'static str
}

#[derive(Serialize)]
pub struct Textbox {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub setting: &'static str
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Widget {
    Dropdown(Dropdown),
    Group(Group),
    Integer(Integer),
    Label(Label),
    #[serde(rename = "zone")] OutputDropdown(OutputDropdown),
    #[serde(rename = "string")] Textbox(Textbox)
}

#[derive(Clone, Debug)]
pub struct Settings {
    save_req_id: Arc<Mutex<Option<usize>>>
}

impl Settings {
    pub fn new(
        roon: &RoonApi,
        get_settings_cb: Box<dyn Fn(fn(serde_json::Value) -> Vec<RespProps>) -> Vec<RespProps> + Send>,
        save_settings_cb: Box<dyn Fn(bool, serde_json::Value) -> Vec<RespProps> + Send>
    ) -> (Svc, Self) {
        let mut spec = SvcSpec::new(SVCNAME);
        let save_req_id = Arc::new(Mutex::new(None));
        let get_settings_cb = Arc::new(Mutex::new(get_settings_cb));

        let get_settings_cb_clone = get_settings_cb.clone();
        let get_settings = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            let get_settings_cb = get_settings_cb_clone.lock().unwrap();

            get_settings_cb(|settings| {
                send_complete!("Success", Some(json!({"settings": settings})))
            })
        };

        spec.add_method("get_settings", Box::new(get_settings));

        let save_req_id_clone = save_req_id.clone();
        let save_settings = move |_: Option<&Core>, req: Option<&serde_json::Value>| -> Vec<RespProps> {
            if let Some(req) = req {
                let is_dry_run = req["body"]["is_dry_run"].as_bool().unwrap();
                let settings = &req["body"]["settings"];

                if !is_dry_run {
                    let mut save_req_id = save_req_id_clone.lock().unwrap();
                    *save_req_id = Some(req["request_id"].as_str().unwrap().parse::<usize>().unwrap());
                }

                save_settings_cb(is_dry_run, settings.to_owned())
            } else {
                vec![(&[], None)]
            }
        };

        spec.add_method("save_settings", Box::new(save_settings));

        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            let get_settings_cb = get_settings_cb.lock().unwrap();

            get_settings_cb(|settings| {
                send_continue!("Subscribed", Some(json!({"settings": settings})))
            })
        };

        spec.add_sub(Sub {
            subscribe_name: "subscribe_settings".to_owned(),
            unsubscribe_name: "unsubscribe_settings".to_owned(),
            start: Box::new(start),
            end: None
        });

        let svc = roon.register_service(spec);
        let settings = Self {
            save_req_id
        };

        (svc, settings)
    }

    pub fn parse_msg(&self, req_id: &usize) -> Parsed {
        let save_req_id = self.save_req_id.lock().unwrap();

        if let Some(save_req_id) = save_req_id.as_ref() {
            if *req_id == *save_req_id {
                return Parsed::SettingsSaved
            }
        }

        Parsed::None
    }
}

#[cfg(test)]
#[cfg(feature = "settings")]
mod tests {
    use crate::{settings, CoreEvent, Info, Services, send_continue_all};

    use super::*;

    fn make_layout(settings: &serde_json::Value) -> serde_json::Value {
        let is_error = false;
        let mut widgets = Vec::new();
        let mut values = Vec::new();

        values.push(HashMap::from([ ("title", "Disabled".into()), ("value", Value::Bool(false)) ]));
        values.push(HashMap::from([ ("title", "Enabled".into()), ("value", Value::Bool(true)) ]));

        let dropdown = Widget::Dropdown(Dropdown {
            title: "Dropdown",
            subtitle: None,
            values,
            setting: "dropdown"
        });

        widgets.push(dropdown);

        json!({
            "values": settings,
            "layout": widgets.serialize(serde_json::value::Serializer).unwrap(),
            "has_error": is_error
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        let info = Info::new("com.theappgineer", "Rust Roon API", "");
        let mut roon = RoonApi::new(info);
        let get_settings = |cb: fn(serde_json::Value) -> Vec<RespProps>| -> Vec<RespProps> {
            let settings = RoonApi::load_config("settings");
            let layout = make_layout(&settings);

            cb(layout)
        };
        let save_settings = |is_dry_run: bool, settings: serde_json::Value| -> Vec<RespProps> {
            let layout = make_layout(&settings["values"]);
            let mut resp_props: Vec<RespProps> = Vec::new();

            send_complete!(resp_props, "Success", Some(json!({"settings": layout})));

            if !is_dry_run && !layout["has_error"].as_bool().unwrap() {
                RoonApi::save_config("settings", layout["values"].to_owned()).unwrap();

                send_continue_all!(resp_props, "subscribe_settings", "Changed", Some(json!({"settings": layout})));
            }

            resp_props
        };
        let (svc, settings) = Settings::new(&roon, Box::new(get_settings), Box::new(save_settings));
        let services = vec![Services::Settings(settings)];
        let mut provided: HashMap<String, Svc> = HashMap::new();

        provided.insert(settings::SVCNAME.to_owned(), svc);

        let (mut handles, mut core_rx) = roon.start_discovery(provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(core) => {
                            println!("Core found: {}, version {}", core.display_name, core.display_version);
                        }
                        CoreEvent::Lost(core) => {
                            println!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }

                    if let Some((_, parsed)) = msg {
                        match parsed {
                            Parsed::SettingsSaved => {
                                println!("Settings saved!");
                            }
                            _ => ()
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
