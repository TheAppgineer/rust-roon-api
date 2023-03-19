use std::sync::{Arc, Mutex};

use crate::{Core, RequiredSvc};

const SVCNAME: &str = "com.roonlabs.transport:2";

pub struct Transport {
    name: &'static str,
    core: Arc<Mutex<Option<Core>>>
}

impl RequiredSvc for Transport {
    fn get_name(&self) -> &str {
        self.name
    }

    fn set_core(&mut self, core: Arc<Mutex<Option<Core>>>) {
        self.core = core;
    }
}

impl Transport {
    pub fn new() -> Self {
        Self {
            name: SVCNAME,
            core: Arc::new(Mutex::new(None))
        }
    }

    pub async fn get_zones(&self) {
        let core = self.core.lock().unwrap();

        if let Some(core) = &*core {
            core.moo.send_req(SVCNAME.to_owned() + "/get_zones", None).await;
        }
    }
}

#[cfg(test)]
#[cfg(feature = "transport")]
mod tests {
    use std::collections::HashMap;
    use serde_json::json;

    use super::*;
    use crate::{RoonApi, Core, Svc};

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        let info = json!({
            "extension_id": "com.theappgineer.rust-roon-api",
            "display_name": "Rust Roon API",
            "display_version": "0.1.0",
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let required: Vec<Box<dyn RequiredSvc>> = vec!(Box::new(Transport::new()));
        let on_core_found = move |core: &Core| {
            println!("Core found: {}, version {}", core.display_name, core.display_version);
        };
        let on_core_lost = move |core: &Core| {
            println!("Core lost: {}", core.display_name);
        };
        let mut roon = RoonApi::new(info, Box::new(on_core_found), Box::new(on_core_lost));
        let mut provided: HashMap<String, Svc> = HashMap::new();

        roon.init_services(&mut provided);

        for handle in roon.start_discovery(provided, required).await.unwrap() {
            handle.await.unwrap();
        }
    }
}
