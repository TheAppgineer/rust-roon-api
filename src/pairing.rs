use json::object;
use std::sync::{Arc, Mutex};

use crate::{RoonApi, MooMsg, SvcSpec, Sub, Svc};

pub struct Pairing {
    pub svc: Option<Svc>,
    paired_core_id: Arc<Mutex<String>>
}

impl Pairing {
    pub fn new() -> Self {
        Self {
            svc: None,
            paired_core_id: Arc::new(Mutex::new(String::new()))
        }
    }

    pub fn register(&mut self, roon: &'static RoonApi) {
        let mut spec = SvcSpec::new();
        let paired_core_id = self.paired_core_id.clone();

        let start = move |req: &mut MooMsg| {
            let paired_core_id = paired_core_id.lock().unwrap();
            let body = object! {
                paired_core_id: paired_core_id.to_owned()
            };
            (req.send_continue.lock().unwrap())("Subscribed", Some(&body)).unwrap();
        };

        let sub = Sub::new("subscribe_pairing", "unsubscribe_pairing", start);
        spec.add_sub(sub);

        let paired_core_id = self.paired_core_id.clone();
        let get_pairing = move |req: &mut MooMsg| {
            let paired_core_id = paired_core_id.lock().unwrap();
            let body = object! {
                paired_core_id: paired_core_id.to_owned()
            };
            (req.send_complete.lock().unwrap())("Success", Some(&body)).unwrap();
        };
        spec.add_method("get_pairing".to_string(), get_pairing);

        let paired_core_id = self.paired_core_id.clone();
        let pair = move |req: &mut MooMsg| {
            let paired_core_id = paired_core_id.lock().unwrap();
            let core_id = roon.get_core_id(req.mooid).unwrap();

            if *paired_core_id != core_id {

            }
        };
        spec.add_method("pair".to_string(), pair);

        self.svc = Some(roon.register_service("com.roonlabs.pairing:1", spec));

    }
}

#[cfg(test)]
mod tests {
    use json::{object, JsonValue};

    use crate::{RoonApi, Core};
    use crate::pairing::Pairing;

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
        let mut svc_pairing = Pairing::new();
        svc_pairing.register(roon_api);

        if let Some(svc) = svc_pairing.svc {
            roon_api.init_services(&[svc]);
        }

        let on_core_paired = move |_core: &Core| {
        };
        let on_core_unpaired = move |_core: &Core| {
        };
        let handle = roon_api.start_discovery(on_core_paired, on_core_unpaired, true);

        handle.join().unwrap();
    }
}
