use std::num::ParseIntError;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    RoonApi,
    Core,
    Parsed,
    RespProps,
    Sub,
    Svc,
    SvcSpec,
    send_complete,
    send_continue,
    send_continue_all,
};

pub const SVCNAME: &str = "com.roonlabs.settings:1";

#[typetag::serde(tag = "type")]
pub trait SerTrait: Send {}

pub type BoxedSerTrait = Box<dyn SerTrait>;

#[derive(Serialize)]
pub struct Dropdown {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub values: Vec<BoxedSerTrait>,
    pub setting: &'static str,
}

#[derive(Serialize)]
pub struct Group {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub collapsable: bool,
    pub items: Vec<Widget>,
}

#[derive(Serialize)]
pub struct Integer {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub min: String,
    pub max: String,
    pub setting: &'static str,
    pub error: Option<String>,
}

impl Integer {
    pub fn out_of_range(&self, value_str: &str) -> Result<bool, ParseIntError> {
        let value = value_str.parse::<i32>()?;
        let min = self.min.parse::<i32>()?;
        let max = self.max.parse::<i32>()?;

        Ok(value < min || value > max)
    }
}

#[derive(Serialize)]
pub struct Label {
    pub title: String,
    pub subtitle: Option<String>,
}

#[derive(Serialize)]
pub struct OutputDropdown {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub setting: &'static str,
}

#[derive(Serialize)]
pub struct Textbox {
    pub title: &'static str,
    pub subtitle: Option<String>,
    pub setting: &'static str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Widget {
    Dropdown(Dropdown),
    Group(Group),
    Integer(Integer),
    Label(Label),
    #[serde(rename = "zone")] OutputDropdown(OutputDropdown),
    #[serde(rename = "string")] Textbox(Textbox),
}

#[derive(Clone, Deserialize, Serialize)]
pub struct OutputSetting {
    pub name: String,
    pub output_id: String,
}

#[derive(Serialize)]
pub struct Layout<T>
where T: serde::ser::Serialize + serde::de::DeserializeOwned,
{
    #[serde(rename = "values")] pub settings: T,
    #[serde(rename = "layout")] pub widgets: Vec<Widget>,
    pub has_error: bool,
}

#[derive(Clone, Debug)]
pub struct Settings {
    save_req_id: Arc<Mutex<Option<usize>>>,
}

impl Settings {
    pub fn new<T>(
        roon: &RoonApi,
        get_layout_cb: Box<dyn Fn(Option<T>) -> Layout<T> + Send>,
    ) -> (Svc, Self)
    where T: serde::ser::Serialize + serde::de::DeserializeOwned + 'static,
    {
        let mut spec = SvcSpec::new(SVCNAME);
        let save_req_id = Arc::new(Mutex::new(None));
        let get_layout_cb = Arc::new(Mutex::new(get_layout_cb));

        let get_layout_cb_clone = get_layout_cb.clone();
        let get_settings = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            let get_layout_cb = get_layout_cb_clone.lock().unwrap();
            let layout = get_layout_cb(None);

            send_complete!("Success", Some(json!({"settings": layout})))
        };

        spec.add_method("get_settings", Box::new(get_settings));

        let get_layout_cb_clone = get_layout_cb.clone();
        let save_req_id_clone = save_req_id.clone();
        let save_settings = move |_: Option<&Core>, req: Option<&serde_json::Value>| -> Vec<RespProps> {
            if let Some(req) = req {
                let is_dry_run = req["body"]["is_dry_run"].as_bool().unwrap();
                let settings = &req["body"]["settings"];

                if !is_dry_run {
                    let mut save_req_id = save_req_id_clone.lock().unwrap();
                    *save_req_id = Some(req["request_id"].as_str().unwrap().parse::<usize>().unwrap());
                }

                let get_layout_cb = get_layout_cb_clone.lock().unwrap();
                let layout = get_layout_cb(
                    Some(serde_json::from_value::<T>(settings["values"].to_owned()).unwrap()),
                );
                let has_error = layout.has_error;
                let layout = layout.serialize(serde_json::value::Serializer).unwrap();
                let mut resp_props: Vec<RespProps> = Vec::new();

                if has_error {
                    send_complete!(resp_props, "NotValid", Some(json!({"settings": layout})));
                } else {
                    send_complete!(resp_props, "Success", Some(json!({"settings": layout})));

                    if !is_dry_run {
                        send_continue_all!(resp_props, "subscribe_settings", "Changed", Some(json!({"settings": layout})));
                    }
                }

                resp_props
            } else {
                vec![(&[], None)]
            }
        };

        spec.add_method("save_settings", Box::new(save_settings));

        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> Vec<RespProps> {
            let get_layout_cb = get_layout_cb.lock().unwrap();
            let layout = get_layout_cb(None);

            send_continue!("Subscribed", Some(json!({"settings": layout})))
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

    pub fn parse_msg(&self, req_id: &usize, body: Option<&serde_json::Value>) -> Parsed {
        if let Some(body) = body {
            let save_req_id = self.save_req_id.lock().unwrap();

            if let Some(save_req_id) = save_req_id.as_ref() {
                if *req_id == *save_req_id {
                    return Parsed::SettingsSaved(body["settings"]["values"].to_owned())
                }
            }
        }

        Parsed::None
    }
}

#[cfg(test)]
#[cfg(feature = "settings")]
mod tests {
    use std::collections::HashMap;
    use crate::{settings, CoreEvent, Info, Services, info};

    use super::*;

    #[derive(Debug, Default, Deserialize, Serialize)]
    struct MySettings {
        state: bool,
        integer: String,
    }

    #[derive(Debug, Default, Deserialize, Serialize)]
    struct Entry {
        title: String,
        value: bool,
    }

    #[typetag::serde]
    impl SerTrait for Entry {}

    impl Entry {
        fn new(title: &str, value: bool) -> BoxedSerTrait {
            Box::new(
                Entry {
                    title: title.to_owned(),
                    value,
                }
            ) as BoxedSerTrait
        }
    }

    fn make_layout(settings: MySettings) -> Layout<MySettings> {
        let mut has_error = false;
        let values = vec![
            Entry::new("Disabled", false),
            Entry::new("Enabled", true),
        ];
        let dropdown = Dropdown {
            title: "Dropdown",
            subtitle: None,
            values,
            setting: "state",
        };
        let mut integer = Integer {
            title: "Integer",
            subtitle: None,
            min: "0".to_owned(),
            max: "100".to_owned(),
            setting: "integer",
            error: None,
        };

        if let Ok(out_of_range) = integer.out_of_range(&settings.integer) {
            if out_of_range {
                integer.error = Some(format!("Value should be between {} and {}", integer.min, integer.max));
                has_error = true;
            }
        }

        let widgets = vec![
            Widget::Dropdown(dropdown),
            Widget::Integer(integer),
        ];

        Layout {
            settings,
            widgets,
            has_error,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";

        simple_logging::log_to_stderr(log::LevelFilter::Info);

        let info = info!("com.theappgineer", "Rust Roon API");
        let mut roon = RoonApi::new(info);
        let get_layout = |settings: Option<MySettings>| -> Layout<MySettings> {
            let settings = match settings {
                Some(settings) => settings,
                None => {
                    let value = RoonApi::load_config(CONFIG_PATH, "settings");

                    serde_json::from_value(value).unwrap_or_default()
                }
            };

            make_layout(settings)
        };
        let (svc, settings) = Settings::new(&roon, Box::new(get_layout));
        let services = vec![Services::Settings(settings)];
        let provided: HashMap<String, Svc> = HashMap::from([
            (settings::SVCNAME.to_owned(), svc),
        ]);
        let get_roon_state = || {
            RoonApi::load_config(CONFIG_PATH, "roonstate")
        };
        let (mut handles, mut core_rx) = roon.start_discovery(
            Box::new(get_roon_state), provided, Some(services)
        ).await.unwrap();

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
                        match parsed {
                            Parsed::RoonState => {
                                RoonApi::save_config(CONFIG_PATH, "roonstate", msg).unwrap();
                            }
                            Parsed::SettingsSaved(settings) => {
                                RoonApi::save_config(CONFIG_PATH, "settings", settings.to_owned()).unwrap();

                                let settings = serde_json::from_value::<MySettings>(settings);
                                log::info!("Settings saved: {:?}", settings);
                            }
                            _ => ()
                        }
                    }
                }
            }
        });

        handles.join_next().await;
    }
}
