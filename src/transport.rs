use serde_json::json;

use crate::Moo;

pub const SVCNAME: &str = "com.roonlabs.transport:2";

#[derive(Clone, Debug)]
pub struct Transport {
    moo: Option<Moo>
}

impl Transport {
    pub fn new() -> Self {
        Self {
            moo: None
        }
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn get_zones(&self) -> Option<usize> {
        if let Some(moo) = &self.moo {
            moo.send_req(SVCNAME.to_owned() + "/get_zones", None).await.ok()
        } else {
            None
        }
    }

    pub async fn subscribe_zones(&self) -> Option<usize> {
        if let Some(moo) = &self.moo {
            moo.send_sub_req(SVCNAME, "zones", None).await.ok()
        } else {
            None
        }
    }

    pub async fn control(&self, zone_id: &str, control: &str) -> Option<usize> {
        if let Some(moo) = &self.moo {
            let body = json!({
                "zone_or_output_id": zone_id,
                "control": control
            });

            moo.send_req(SVCNAME.to_owned() + "/control", Some(body)).await.ok()
        } else {
            None
        }
    }
}

#[cfg(test)]
#[cfg(feature = "transport")]
mod tests {
    use std::collections::HashMap;
    use serde_json::json;

    use super::*;
    use crate::{RoonApi, Core, Svc, Services};

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        let info = json!({
            "extension_id": "com.theappgineer.rust-roon-api",
            "display_name": "Rust Roon API",
            "display_version": "0.1.0",
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let services = vec![Services::Transport(Transport::new())];
        let on_core_lost = move |core: &Core| {
            println!("Core lost: {}", core.display_name);
        };
        let mut roon = RoonApi::new(info, Box::new(on_core_lost));
        let provided: HashMap<String, Svc> = HashMap::new();
        let (mut handles, mut core_rx) = roon.start_discovery(provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            let mut transport = None;

            loop {
                if let Some((mut core, msg)) = core_rx.recv().await {
                    if let Some(core) = core.as_mut() {
                        println!("Core found: {}, version {}", core.display_name, core.display_version);

                        transport = if let Some(transport) = core.get_transport() {
                            transport.subscribe_zones().await;
                            Some(transport.clone())
                        } else {
                            None
                        }
                    }

                    if let Some((response, body)) = msg {
                        if response == "Subscribed" {
                            if let Some(transport) = &transport {
                                let zone_id = body["zones"][1]["zone_id"].as_str().unwrap();

                                transport.control(zone_id, "playpause").await;
                            }
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
