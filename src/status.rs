use std::sync::{Arc, Mutex};

use json::object;

use crate::{RoonApi, MooMsg, SvcSpec, Sub, Svc};

pub struct Status {
    pub svc: Svc,
    props: Arc<Mutex<(String, bool)>>
}

impl Status {
    pub fn new(roon: &RoonApi) -> Self {
        let props = Arc::new(Mutex::new((String::new(), false)));
        let mut spec = SvcSpec::new();

        {
            let props = props.clone();
            let start = move |req: &mut MooMsg| {
                let props = props.lock().unwrap();
                let body = object! {
                    message: props.0.to_string(),
                    is_error: props.1
                };
                (req.send_continue.lock().unwrap())("Subscribed", Some(&body)).unwrap();
            };

            let sub = Sub::new("subscribe_status", "unsubscribe_status", start);
            spec.add_sub(sub);
        }

        {
            let props = props.clone();
            let get_status = move |req: &mut MooMsg| {
                let props = props.lock().unwrap();
                let body = object! {
                    message:  props.0.to_string(),
                    is_error: props.1
                };
                (req.send_complete.lock().unwrap())("Success", Some(&body)).unwrap();
            };
            spec.add_method("get_status".to_string(), get_status);
        }
        let svc = roon.register_service("com.roonlabs.status:1", spec);

        Self {
            svc,
            props
        }
    }

    pub fn set_status(&mut self, message: &str, is_error: bool) {
        let mut props = self.props.lock().unwrap();
        let send_continue_all = self.svc.send_continue_all.lock().unwrap();

        props.0 = message.to_owned();
        props.1 = is_error;

        let body = object! {
            message:  props.0.clone(),
            is_error: props.1
        };

        if let Err(error) = (send_continue_all)("subscribe_status", "Changed", body) {
            println!("{}", error);
        }
    }
}

#[cfg(test)]
mod tests {
    use json::{object, JsonValue};
    use std::sync::{Arc, Mutex};

    use crate::{RoonApi, Core};
    use crate::status::Status;

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
        let svc_status = Arc::new(Mutex::new(Status::new(&roon_api)));

        roon_api.init_services(&[svc_status.lock().unwrap().svc.clone()]);

        let clone = svc_status.clone();
        let on_core_found = move |core: &Core| {
            let message = format!("Core found: {}", core.display_name);
            let mut svc_status = clone.lock().unwrap();
            svc_status.set_status(&message, false);
        };
        let on_core_lost = move |core: &Core| {
            let message = format!("Core lost: {}", core.display_name);
            let mut svc_status = svc_status.lock().unwrap();
            svc_status.set_status(&message, false);
        };
        let handle = roon_api.start_discovery(on_core_found, on_core_lost);

        handle.join().unwrap();
    }
}
