use std::sync::{Arc, Mutex};
use serde_json::json;

use crate::{RoonApi, Core, RespProps, Sub, Svc, SvcSpec};

pub const SVCNAME: &str = "com.roonlabs.status:1";

pub struct Status {
    props: Arc<Mutex<(String, bool)>>
}

impl Status {
    pub fn new() -> Self {
        Self {
            props: Arc::new(Mutex::new((String::new(), false)))
        }
    }

    pub fn get_service(&self, roon: &RoonApi) -> Svc {
        let mut spec = SvcSpec::new(SVCNAME);

        let props_clone = self.props.clone();
        let get_status = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            let (message, is_error) = &*props_clone.lock().unwrap();
            let body = json!({
                "message": message,
                "is_error": is_error
            });

            (&["COMPLETE", "Success"], Some(body))
        };

        spec.add_method("get_status", Box::new(get_status));

        let props_clone = self.props.clone();
        let start = move |_: Option<&Core>, _: Option<&serde_json::Value>| -> RespProps {
            let (message, is_error) = &*props_clone.lock().unwrap();
            let body = json!({
                "message": message,
                "is_error": is_error
            });

            (&["CONTINUE", "Subscribed"], Some(body))
        };

        spec.add_sub(Sub {
            subscribe_name: "subscribe_status".to_owned(),
            unsubscribe_name: "unsubscribe_status".to_owned(),
            start: Box::new(start),
            end: None
        });

        roon.register_service(spec)
    }

    pub fn set_status(&self, message: String, is_error: bool) -> RespProps {
        let mut props = self.props.lock().unwrap();
        let body = json!({
            "message": message,
            "is_error": is_error
        });

        *props = (message, is_error);
        (&["subscribe_status", "CONTINUE", "Changed"], Some(body))
    }
}

#[cfg(test)]
#[cfg(feature = "status")]
mod tests {
    use std::collections::HashMap;
    use serde_json::json;

    use crate::status::{self, Status};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        let info = json!({
            "extension_id": "com.theappgineer.rust-roon-api",
            "display_name": "Rust Roon API",
            "display_version": "0.1.0",
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let status = Arc::new(Status::new());

        let status_clone = status.clone();
        let on_core_lost = move |core: &Core| {
            let message = format!("Core lost: {}", core.display_name);
            status_clone.set_status(message, false);
        };

        let mut roon = RoonApi::new(info, Box::new(on_core_lost));
        let mut provided: HashMap<String, Svc> = HashMap::new();
        let svc = status.get_service(&roon);

        provided.insert(status::SVCNAME.to_owned(), svc);

        let (mut handles, mut core_rx) = roon.start_discovery(provided).await.unwrap();
        let status_clone = status.clone();

        handles.push(tokio::spawn(async move {
            if let Some((core, _)) = core_rx.recv().await {
                if let Some(core) = core {
                    let message = format!("Core found: {}\nversion {}", core.display_name, core.display_version);
                    status_clone.set_status(message, false);
                }
            }
        }));

        for handle in handles {
            handle.await.unwrap();
        }
    }
}
