
use json::{JsonValue, object};
use std::sync::{Arc, Mutex};

use crate::{RoonApi, MooMsg, SvcSpec, Sub, Svc};

pub struct Settings {
    pub svc: Option<Svc>,
}

impl Settings {
    pub fn new() -> Self {
        Self {
            svc: None
        }
    }

    pub fn register<F, G>(&mut self, roon: &RoonApi, get_settings: F, save_settings: G)
    where F: Fn(Box<dyn Fn(JsonValue)>) + Send + 'static,
          G: Fn(&MooMsg, bool, &JsonValue) + Send + 'static
    {
        let mut spec = SvcSpec::new();
        let get_settings = Arc::new(Mutex::new(get_settings));

        {
            let get_settings = get_settings.clone();
            let start = move |req: &mut MooMsg| {
                let req = req.clone();
                let cb = move |settings: JsonValue| {
                    let body = object! { settings: settings };
                    (req.send_continue.lock().unwrap())("Subscribed", Some(&body)).unwrap();
                };

                (get_settings.lock().unwrap())(Box::new(cb));
            };

            let sub = Sub::new("subscribe_settings", "unsubscribe_settings", start);
            spec.add_sub(sub);
        }

        let get_settings_method = move |req: &mut MooMsg| {
            let req = req.clone();
            let cb = move |settings: JsonValue| {
                let body = object! { settings: settings };
                (req.send_complete.lock().unwrap())("Success", Some(&body)).unwrap();
            };

            (get_settings.lock().unwrap())(Box::new(cb));
        };
        spec.add_method("get_settings".to_string(), get_settings_method);

        let save_settings_method = move |req: &mut MooMsg| {
            let body = &req.msg["body"];
            (save_settings)(req, body["is_dry_run"].as_bool().unwrap(), &body["settings"]);
        };
        spec.add_method("save_settings".to_string(), save_settings_method);

        self.svc = Some(roon.register_service("com.roonlabs.settings:1", spec));
    }

    pub fn update_settings(&self, settings: JsonValue) {
        if let Some(svc) = &self.svc {
            let send_continue_all = svc.send_continue_all.lock().unwrap();
            let body = object! { settings: settings };
    
            if let Err(error) = (send_continue_all)("subscribe_settings", "Changed", body) {
                println!("{}", error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use json::{object, JsonValue, array};
    use std::sync::{Arc, Mutex};

    use crate::{RoonApi, MooMsg};
    use crate::settings::Settings;

    #[test]
    fn it_works() -> Result<(), Box<dyn std::error::Error>> {
        let ext_opts: JsonValue = object! {
            extension_id:    "com.theappgineer.rust-roon-api",
            display_name:    "Rust Roon API",
            display_version: "0.1.0",
            publisher:       "The Appgineer",
            email:           "theappgineer@gmail.com",
            log_level:       "all"
        };
        let roon_api = Box::new(RoonApi::new(ext_opts));
        // Leak the RoonApi instance to give it a 'static lifetime
        let roon_api: &'static mut RoonApi = Box::leak(roon_api);
        let my_settings = Arc::new(Mutex::new(RoonApi::load_config(Some("settings"))?));

        let svc_settings = Arc::new(Mutex::new(Settings::new()));
        let my_settings_clone = my_settings.clone();
        let get_settings = move |cb: Box<dyn Fn(JsonValue)>| {
            (cb)(makelayout(&my_settings_clone.lock().unwrap()));
        };
        let svc_settings_clone = svc_settings.clone();
        let save_settings = move |req: &MooMsg, is_dry_run: bool, settings: &JsonValue| {
            let layout = makelayout(&settings["values"]);
            let status = if layout["has_error"].as_bool().unwrap() {"NotValid"} else {"Success"};

            (req.send_complete.lock().unwrap())(status, Some(&object! { settings: layout.clone() })).unwrap();

            if !is_dry_run && status == "Success" {
                let mut my_settings = my_settings.lock().unwrap();
                *my_settings = layout["values"].to_owned();
                svc_settings_clone.lock().unwrap().update_settings(layout);
                RoonApi::save_config("settings", Some(my_settings.to_owned())).unwrap();
            }
        };
        svc_settings.lock().unwrap().register(&roon_api, get_settings, save_settings);

        if let Some(svc) = &svc_settings.lock().unwrap().svc {
            roon_api.init_services(&[svc.clone()]);
        }

        let handle = roon_api.start_discovery(|_|(), |_|(), false);

        handle.join().unwrap();

        Ok(())
    }

    fn makelayout(settings: &JsonValue) -> JsonValue {
        let mut layout = object! {
            values: settings.to_owned(),
            layout: array![],
            has_error: false
        };
        let dropdown = object! {
            "type": "dropdown",
            title: "Rust Settings API dropdown",
            values: array![
                object! { title: "Disabled", value: false },
                object! { title: "Enabled", value: true }
            ],
            setting: "dropdown"
        };
        layout["layout"].push(dropdown).unwrap();

        layout
    }
}
