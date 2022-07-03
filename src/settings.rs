
use json::{JsonValue, object};
use std::sync::{Arc, Mutex};

use crate::{RoonApi, MooMsg, SvcSpec, Sub, Svc};

pub struct Settings {
    pub svc: Svc,
}

impl Settings {
    pub fn new<F, G>(roon: &RoonApi, get_settings: F, save_settings: G) -> Self
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

        let svc = roon.register_service("com.roonlabs.settings:1", spec);

        Self {
            svc
        }
    }

    pub fn update_settings(&self, settings: JsonValue) {
        let send_continue_all = self.svc.send_continue_all.lock().unwrap();
        let body = object! { settings: settings };

        if let Err(error) = (send_continue_all)("subscribe_settings", "Changed", body) {
            println!("{}", error);
        }
    }
}

#[cfg(test)]
mod tests {
    use json::{object, JsonValue, array};
    use std::sync::{Arc, Mutex};

    use crate::{RoonApi, Core, MooMsg};
    use crate::settings::Settings;

    #[test]
    fn it_works() {
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
        let get_settings = |cb: Box<dyn Fn(JsonValue)>| {
            let settings = object! {};

            (cb)(makelayout(settings));
        };
        let save_settings = |req: &MooMsg, is_dry_run: bool, settings: &JsonValue| {
            let layout = makelayout(settings["values"].to_owned());
            let status = if layout["has_error"].as_bool().unwrap() {"NotValid"} else {"Success"};

            (req.send_complete.lock().unwrap())(status, Some(&object! { settings: layout })).unwrap();

            if !is_dry_run && status == "Success" {
            }
        };
        let svc_settings = Arc::new(Mutex::new(Settings::new(&roon_api, get_settings, save_settings)));

        roon_api.init_services(&[svc_settings.lock().unwrap().svc.clone()]);

        let on_core_found = move |_core: &Core| {
        };
        let on_core_lost = move |_core: &Core| {
        };
        let handle = roon_api.start_discovery(on_core_found, on_core_lost);

        handle.join().unwrap();
    }

    fn makelayout(settings: JsonValue) -> JsonValue {
        let mut layout = object! {
            values: settings,
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
